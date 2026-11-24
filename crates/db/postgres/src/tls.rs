//! PostgreSQL TLS：用 rustls 实现 `tokio_postgres` 的 `MakeTlsConnect`。
//!
//! `tokio-postgres` 只提供 TLS 抽象，具体实现通常由 `postgres-native-tls` /
//! `tokio-postgres-rustls` 提供。这里直接基于 `rustls` + `tokio-rustls` 实现，
//! 好处是根证书来源、校验策略（`sslmode`）都掌握在自己手里，且与 SQL Server /
//! Oracle 共用同一个 crypto provider（aws-lc-rs），不会出现双 provider panic。

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_postgres::tls::{ChannelBinding, MakeTlsConnect, TlsConnect, TlsStream};

/// 按 `sslmode` 构造 TLS 连接器。
#[derive(Clone)]
pub struct RustlsMakeTlsConnect {
    config: Arc<ClientConfig>,
}

impl RustlsMakeTlsConnect {
    /// `verify = false` 对应 `sslmode=require`：只加密、不校验证书。
    ///
    /// `verify = true` 用系统根证书做完整校验（含主机名）。
    pub fn new(verify: bool) -> Result<Self, String> {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let builder = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS12, &rustls::version::TLS13])
            .map_err(|err| format!("TLS 配置失败：{err}"))?;

        let config = if verify {
            let loaded = rustls_native_certs::load_native_certs();
            if !loaded.errors.is_empty() {
                log::warn!(
                    "读取系统根证书时有 {} 个错误（首个：{:?}）",
                    loaded.errors.len(),
                    loaded.errors.first()
                );
            }
            let mut roots = RootCertStore::empty();
            for cert in loaded.certs {
                // 单张证书损坏不影响其余根证书。
                let _ = roots.add(cert);
            }
            if roots.is_empty() {
                return Err("没有可用的系统根证书，无法校验 PostgreSQL 服务端证书".into());
            }
            builder.with_root_certificates(roots).with_no_client_auth()
        } else {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerify))
                .with_no_client_auth()
        };
        Ok(Self {
            config: Arc::new(config),
        })
    }
}

impl<S> MakeTlsConnect<S> for RustlsMakeTlsConnect
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = RustlsStream<S>;
    type TlsConnect = RustlsTlsConnect;
    type Error = io::Error;

    fn make_tls_connect(&mut self, domain: &str) -> Result<Self::TlsConnect, io::Error> {
        Ok(RustlsTlsConnect {
            config: self.config.clone(),
            domain: domain.to_string(),
        })
    }
}

/// 每次握手用的连接器；`domain` 用作 SNI 与证书主机名校验。
pub struct RustlsTlsConnect {
    config: Arc<ClientConfig>,
    domain: String,
}

impl<S> TlsConnect<S> for RustlsTlsConnect
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = RustlsStream<S>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Stream, io::Error>> + Send>>;

    fn connect(self, stream: S) -> Self::Future {
        Box::pin(async move {
            let name = ServerName::try_from(self.domain).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "PostgreSQL 主机名不合法")
            })?;
            let connector = tokio_rustls::TlsConnector::from(self.config);
            let tls = connector
                .connect(name, stream)
                .await
                .map_err(|err| io::Error::other(format!("TLS 握手失败：{err}")))?;
            Ok(RustlsStream(tls))
        })
    }
}

/// `tokio_postgres` 只接受实现了它自己的 `TlsStream` 的类型；
/// `tokio_rustls` 的流是外部类型，所以套一层 newtype 做转发。
pub struct RustlsStream<S>(tokio_rustls::client::TlsStream<S>);

impl<S> AsyncRead for RustlsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl<S> AsyncWrite for RustlsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.0.is_write_vectored()
    }
}

impl<S> TlsStream for RustlsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn channel_binding(&self) -> ChannelBinding {
        // 未实现 tls-server-end-point：SCRAM 会退回不带通道绑定的模式。
        ChannelBinding::none()
    }
}

/// `sslmode=require`：加密但不校验证书（与 libpq 语义一致）。
#[derive(Debug)]
struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_mode_builds_without_system_roots() {
        // require 不读系统根证书，任何平台都应成功。
        assert!(RustlsMakeTlsConnect::new(false).is_ok());
    }

    #[test]
    fn verify_mode_reports_missing_roots_instead_of_panicking() {
        // 只断言不 panic：有系统根证书时返回 Ok，没有时返回可读错误。
        match RustlsMakeTlsConnect::new(true) {
            Ok(_) => {}
            Err(message) => assert!(message.contains("根证书"), "{message}"),
        }
    }
}
