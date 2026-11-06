//! 例程源码（视图 / 存储过程 / 函数 / 触发器 / 程序包）的查看与保存策略（F-82）。
//!
//! 纯逻辑，不做 IO：方言差异全部收敛在这里，UI 只按返回值渲染，不做 `match DbKind`。

use rustdb_db_api::DbKind;

/// 可查看 / 可编辑源码的例程种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineKind {
    View,
    MaterializedView,
    Procedure,
    Function,
    Trigger,
    Package,
}

impl RoutineKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::View => "视图",
            Self::MaterializedView => "物化视图",
            Self::Procedure => "存储过程",
            Self::Function => "函数",
            Self::Trigger => "触发器",
            Self::Package => "程序包",
        }
    }

    /// DDL 里的对象关键字（拼 `DROP` 语句时用）。
    pub fn keyword(self) -> &'static str {
        match self {
            Self::View => "VIEW",
            Self::MaterializedView => "MATERIALIZED VIEW",
            Self::Procedure => "PROCEDURE",
            Self::Function => "FUNCTION",
            Self::Trigger => "TRIGGER",
            Self::Package => "PACKAGE",
        }
    }
}

/// 保存方式：不同方言对「重复创建」的处理不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStrategy {
    /// `CREATE OR REPLACE`（PostgreSQL / Oracle / MySQL 视图）。
    CreateOrReplace,
    /// `CREATE OR ALTER`（SQL Server 2016 SP1+）。
    CreateOrAlter,
    /// 先 `DROP` 再 `CREATE`：会丢掉视图 / 例程的依赖，必须显式警告。
    DropAndCreate,
    /// 该方言不支持保存源码（只能查看）。
    Unsupported,
}

impl SaveStrategy {
    pub fn label(self) -> &'static str {
        match self {
            Self::CreateOrReplace => "CREATE OR REPLACE",
            Self::CreateOrAlter => "CREATE OR ALTER",
            Self::DropAndCreate => "先 DROP 再 CREATE",
            Self::Unsupported => "不支持保存",
        }
    }

    /// 会不会先删掉现有对象（依赖会丢）。
    pub fn destructive(self) -> bool {
        matches!(self, Self::DropAndCreate)
    }
}

/// 驱动返回的源码形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceShape {
    /// 完整 `CREATE` 语句（改写 `CREATE` 关键字即可）。
    FullStatement,
    /// 只有例程体（需要按方言补全签名）。
    BodyOnly,
}

/// 某方言 + 某例程的支持情况。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineSupport {
    pub strategy: SaveStrategy,
    pub shape: SourceShape,
    /// 不能保存时的原因（显式降级，不静默失败）。
    pub reason: Option<String>,
}

impl RoutineSupport {
    pub fn can_save(&self) -> bool {
        self.strategy != SaveStrategy::Unsupported
    }
}

/// 该方言 + 该例程的保存能力。
pub fn routine_support(kind: DbKind, routine: RoutineKind) -> RoutineSupport {
    use RoutineKind as R;
    use SaveStrategy as S;
    use SourceShape as Shape;

    let (strategy, shape) = match (kind, routine) {
        // PostgreSQL：视图 / 函数 / 过程都能 CREATE OR REPLACE；触发器不行。
        (DbKind::Postgres, R::View | R::Function | R::Procedure) => {
            (S::CreateOrReplace, Shape::FullStatement)
        }
        (DbKind::Postgres, R::Trigger) => (S::DropAndCreate, Shape::FullStatement),
        // Oracle：视图 / 过程 / 函数 / 触发器 / 程序包都是 CREATE OR REPLACE。
        (DbKind::Oracle, R::View | R::Procedure | R::Function | R::Trigger | R::Package) => {
            (S::CreateOrReplace, Shape::FullStatement)
        }
        // MySQL：只有视图支持 OR REPLACE，过程 / 函数 / 触发器要先删。
        (DbKind::Mysql | DbKind::Mariadb, R::View) => (S::CreateOrReplace, Shape::FullStatement),
        (DbKind::Mysql | DbKind::Mariadb, R::Procedure | R::Function | R::Trigger) => {
            (S::DropAndCreate, Shape::FullStatement)
        }
        // SQL Server：OBJECT_DEFINITION 只给例程体，视图 / 过程能靠 CREATE OR ALTER 补签名。
        (DbKind::Mssql, R::View | R::Procedure) => (S::CreateOrAlter, Shape::BodyOnly),
        // SQLite：视图 / 触发器只能删了重建。
        (DbKind::Sqlite, R::View | R::Trigger) => (S::DropAndCreate, Shape::FullStatement),
        _ => (S::Unsupported, Shape::FullStatement),
    };

    let reason = if strategy == S::Unsupported {
        Some(match routine {
            R::MaterializedView => {
                format!("{} 不支持保存物化视图源码（仅可查看）", kind.display_name())
            }
            R::Function if kind == DbKind::Mssql => {
                "SQL Server 只返回函数体，无法还原函数签名，不能安全保存".into()
            }
            R::Trigger if kind == DbKind::Mssql => {
                "SQL Server 只返回触发器体，缺少所属表，无法安全保存".into()
            }
            _ => format!(
                "{} 不支持保存{}源码（仅可查看）",
                kind.display_name(),
                routine.label()
            ),
        })
    } else {
        None
    };

    RoutineSupport {
        strategy,
        shape,
        reason,
    }
}

/// 保存计划：语句 + 策略 + 警告 / 失败原因。
#[derive(Debug, Clone, PartialEq)]
pub struct RoutineSavePlan {
    pub statements: Vec<String>,
    pub strategy: SaveStrategy,
    /// 需要用户确认的警告（DROP + CREATE 会丢依赖）。
    pub warning: Option<String>,
    /// 无法保存的原因（显式降级）。
    pub error: Option<String>,
}

impl RoutineSavePlan {
    fn fail(strategy: SaveStrategy, error: impl Into<String>) -> Self {
        Self {
            statements: Vec::new(),
            strategy,
            warning: None,
            error: Some(error.into()),
        }
    }

    pub fn is_ok(&self) -> bool {
        self.error.is_none() && !self.statements.is_empty()
    }

    /// 预览文本：语句逐行，警告单独列出。
    pub fn preview_text(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        if let Some(warning) = &self.warning {
            lines.push(format!("-- ⚠ {warning}"));
        }
        lines.extend(self.statements.iter().cloned());
        lines.join("\n")
    }
}

/// 生成保存源码要执行的语句。
///
/// `qualified` 是方言限定名（已带引用符），`source` 是用户编辑后的源码。
pub fn plan_save(
    kind: DbKind,
    routine: RoutineKind,
    qualified: &str,
    source: &str,
) -> RoutineSavePlan {
    let support = routine_support(kind, routine);
    if !support.can_save() {
        return RoutineSavePlan::fail(
            support.strategy,
            support
                .reason
                .unwrap_or_else(|| "该对象不支持保存源码".into()),
        );
    }

    let trimmed = source.trim();
    if trimmed.is_empty() {
        return RoutineSavePlan::fail(support.strategy, "源码为空，没有可执行的语句");
    }

    match support.shape {
        SourceShape::BodyOnly => {
            // 驱动只给例程体：按方言补出完整语句。
            let keyword = routine.keyword();
            let body = trimmed;
            let modifier = match support.strategy {
                SaveStrategy::CreateOrAlter => "OR ALTER",
                SaveStrategy::CreateOrReplace => "OR REPLACE",
                _ => "OR ALTER",
            };
            let statement = format!("CREATE {modifier} {keyword} {qualified} AS {body}");
            RoutineSavePlan {
                statements: vec![statement],
                strategy: support.strategy,
                warning: None,
                error: None,
            }
        }
        SourceShape::FullStatement => {
            let body = strip_leading_comments(trimmed);
            if !body.to_ascii_uppercase().starts_with("CREATE") {
                return RoutineSavePlan::fail(
                    support.strategy,
                    "源码不是完整的 CREATE 语句，无法自动包装保存",
                );
            }
            match support.strategy {
                SaveStrategy::CreateOrReplace => match with_modifier(trimmed, "OR REPLACE") {
                    Some(statement) => RoutineSavePlan {
                        statements: vec![statement],
                        strategy: support.strategy,
                        warning: None,
                        error: None,
                    },
                    None => RoutineSavePlan::fail(
                        support.strategy,
                        "无法识别源码开头的 CREATE 关键字，未生成语句",
                    ),
                },
                SaveStrategy::CreateOrAlter => match with_modifier(trimmed, "OR ALTER") {
                    Some(statement) => RoutineSavePlan {
                        statements: vec![statement],
                        strategy: support.strategy,
                        warning: None,
                        error: None,
                    },
                    None => RoutineSavePlan::fail(
                        support.strategy,
                        "无法识别源码开头的 CREATE 关键字，未生成语句",
                    ),
                },
                SaveStrategy::DropAndCreate => {
                    let drop = match drop_statement(kind, routine, qualified, trimmed) {
                        Ok(sql) => sql,
                        Err(message) => return RoutineSavePlan::fail(support.strategy, message),
                    };
                    RoutineSavePlan {
                        statements: vec![drop, trimmed.to_string()],
                        strategy: support.strategy,
                        warning: Some(
                            "该方言不支持 CREATE OR REPLACE，将先删除再重建（依赖会丢失，请先备份）"
                                .into(),
                        ),
                        error: None,
                    }
                }
                SaveStrategy::Unsupported => RoutineSavePlan::fail(support.strategy, "不支持保存"),
            }
        }
    }
}

/// 生成「先删后建」里的 `DROP` 语句。
fn drop_statement(
    kind: DbKind,
    routine: RoutineKind,
    qualified: &str,
    source: &str,
) -> Result<String, String> {
    // PostgreSQL 的触发器必须写成 `DROP TRIGGER … ON 表`，表名只能从源码里解析。
    if kind == DbKind::Postgres && routine == RoutineKind::Trigger {
        let (name, table) = trigger_target(source)
            .ok_or_else(|| "无法从源码解析触发器名与所属表，请先手动删除触发器".to_string())?;
        return Ok(format!("DROP TRIGGER {name} ON {table};"));
    }
    Ok(format!("DROP {} IF EXISTS {qualified};", routine.keyword()))
}

/// 在 `CREATE` 关键字后插入包装词；已经带了就原样返回。
fn with_modifier(source: &str, modifier: &str) -> Option<String> {
    let trimmed = source.trim();
    let body = strip_leading_comments(trimmed);
    // `body` 是 `trimmed` 的后缀切片，指针差即它在 `trimmed` 里的字节偏移。
    let offset = body.as_ptr() as usize - trimmed.as_ptr() as usize;
    let head = body.get(..6)?;
    if !head.eq_ignore_ascii_case("CREATE") {
        return None;
    }
    let rest = &body[6..];
    let gap_len = rest.len() - rest.trim_start().len();
    let rest_trimmed = &rest[gap_len..];
    for existing in ["OR REPLACE", "OR ALTER"] {
        if rest_trimmed
            .get(..existing.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(existing))
        {
            return Some(trimmed.to_string());
        }
    }
    // 注释原样保留，只在 `CREATE` 后面插包装词。
    let before = trimmed.get(..offset + 6)?;
    Some(format!(
        "{before} {modifier}{}{rest_trimmed}",
        &rest[..gap_len]
    ))
}

/// 跳过开头的注释（`--` 行注释 / `/* */` 块注释）。
fn strip_leading_comments(source: &str) -> &str {
    let mut rest = source.trim_start();
    loop {
        if let Some(tail) = rest.strip_prefix("--") {
            match tail.find('\n') {
                Some(index) => rest = tail[index + 1..].trim_start(),
                None => return "",
            }
        } else if let Some(tail) = rest.strip_prefix("/*") {
            match tail.find("*/") {
                Some(index) => rest = tail[index + 2..].trim_start(),
                None => return "",
            }
        } else {
            return rest;
        }
    }
}

/// 从 `CREATE TRIGGER` 源码里取出触发器名与所属表。
fn trigger_target(source: &str) -> Option<(String, String)> {
    let body = strip_leading_comments(source);
    let tokens: Vec<&str> = body.split_whitespace().collect();
    let trigger_at = tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case("TRIGGER"))?;
    let name = tokens.get(trigger_at + 1)?;
    let on_at = tokens
        .iter()
        .skip(trigger_at + 2)
        .position(|token| token.eq_ignore_ascii_case("ON"))?
        + trigger_at
        + 2;
    let table = tokens.get(on_at + 1)?;
    Some((name.to_string(), table.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg() -> DbKind {
        DbKind::Postgres
    }

    #[test]
    fn postgres_uses_create_or_replace_for_views_and_routines() {
        assert_eq!(
            routine_support(pg(), RoutineKind::View).strategy,
            SaveStrategy::CreateOrReplace
        );
        assert_eq!(
            routine_support(pg(), RoutineKind::Function).strategy,
            SaveStrategy::CreateOrReplace
        );
        assert_eq!(
            routine_support(pg(), RoutineKind::Procedure).strategy,
            SaveStrategy::CreateOrReplace
        );
        assert_eq!(
            routine_support(pg(), RoutineKind::Trigger).strategy,
            SaveStrategy::DropAndCreate
        );
        assert!(!routine_support(pg(), RoutineKind::MaterializedView).can_save());
    }

    #[test]
    fn mysql_routines_drop_before_create() {
        assert_eq!(
            routine_support(DbKind::Mysql, RoutineKind::View).strategy,
            SaveStrategy::CreateOrReplace
        );
        assert_eq!(
            routine_support(DbKind::Mysql, RoutineKind::Procedure).strategy,
            SaveStrategy::DropAndCreate
        );
        assert_eq!(
            routine_support(DbKind::Mariadb, RoutineKind::Trigger).strategy,
            SaveStrategy::DropAndCreate
        );
    }

    #[test]
    fn sqlite_view_and_trigger_drop_first() {
        assert_eq!(
            routine_support(DbKind::Sqlite, RoutineKind::View).strategy,
            SaveStrategy::DropAndCreate
        );
        assert_eq!(
            routine_support(DbKind::Sqlite, RoutineKind::Trigger).strategy,
            SaveStrategy::DropAndCreate
        );
        assert!(!routine_support(DbKind::Sqlite, RoutineKind::Function).can_save());
    }

    #[test]
    fn mssql_functions_and_triggers_are_explicitly_degraded() {
        let function = routine_support(DbKind::Mssql, RoutineKind::Function);
        assert!(!function.can_save());
        assert!(function.reason.is_some());
        let trigger = routine_support(DbKind::Mssql, RoutineKind::Trigger);
        assert!(!trigger.can_save());
        assert!(trigger.reason.is_some());
    }

    #[test]
    fn oracle_supports_every_editable_routine() {
        for routine in [
            RoutineKind::View,
            RoutineKind::Procedure,
            RoutineKind::Function,
            RoutineKind::Trigger,
            RoutineKind::Package,
        ] {
            assert_eq!(
                routine_support(DbKind::Oracle, routine).strategy,
                SaveStrategy::CreateOrReplace
            );
        }
    }

    #[test]
    fn create_or_replace_is_inserted_after_create() {
        let plan = plan_save(
            pg(),
            RoutineKind::View,
            "\"public\".\"v\"",
            "CREATE VIEW public.v AS SELECT 1;",
        );
        assert!(plan.is_ok());
        assert_eq!(
            plan.statements,
            vec!["CREATE OR REPLACE VIEW public.v AS SELECT 1;".to_string()]
        );
        assert!(plan.warning.is_none());
    }

    #[test]
    fn existing_or_replace_is_not_duplicated() {
        let plan = plan_save(
            pg(),
            RoutineKind::Function,
            "\"public\".\"f\"",
            "CREATE OR REPLACE FUNCTION public.f() RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql;",
        );
        assert_eq!(plan.statements.len(), 1);
        assert_eq!(plan.statements[0].matches("OR REPLACE").count(), 1);
    }

    #[test]
    fn mysql_definer_prefix_keeps_working() {
        let plan = plan_save(
            DbKind::Mysql,
            RoutineKind::View,
            "`shop`.`v`",
            "CREATE ALGORITHM=UNDEFINED DEFINER=`root`@`localhost` SQL SECURITY DEFINER VIEW `v` AS SELECT 1;",
        );
        assert!(plan.is_ok());
        assert!(plan.statements[0].starts_with("CREATE OR REPLACE ALGORITHM=UNDEFINED"));
    }

    #[test]
    fn drop_and_create_emits_warning_and_two_statements() {
        let plan = plan_save(
            DbKind::Mysql,
            RoutineKind::Procedure,
            "`shop`.`p`",
            "CREATE PROCEDURE `p`() BEGIN SELECT 1; END",
        );
        assert!(plan.is_ok());
        assert_eq!(plan.statements.len(), 2);
        assert_eq!(plan.statements[0], "DROP PROCEDURE IF EXISTS `shop`.`p`;");
        assert!(plan.warning.unwrap().contains("先删除再重建"));
    }

    #[test]
    fn postgres_trigger_drop_includes_owning_table() {
        let plan = plan_save(
            pg(),
            RoutineKind::Trigger,
            "\"public\".\"t\"",
            "CREATE TRIGGER t BEFORE INSERT ON public.account FOR EACH ROW EXECUTE FUNCTION f();",
        );
        assert!(plan.is_ok());
        assert_eq!(plan.statements[0], "DROP TRIGGER t ON public.account;");
    }

    #[test]
    fn unparsable_trigger_reports_error_instead_of_wrong_sql() {
        let plan = plan_save(
            pg(),
            RoutineKind::Trigger,
            "\"public\".\"t\"",
            "CREATE TRIGGER t;",
        );
        assert!(!plan.is_ok());
        assert!(plan.error.unwrap().contains("所属表"));
    }

    #[test]
    fn mssql_body_only_source_gets_signature() {
        let plan = plan_save(
            DbKind::Mssql,
            RoutineKind::View,
            "[shop].[dbo].[v]",
            "SELECT id FROM dbo.account;",
        );
        assert!(plan.is_ok());
        assert_eq!(
            plan.statements[0],
            "CREATE OR ALTER VIEW [shop].[dbo].[v] AS SELECT id FROM dbo.account;"
        );
    }

    #[test]
    fn empty_or_partial_source_is_rejected() {
        let empty = plan_save(pg(), RoutineKind::View, "\"v\"", "   ");
        assert!(!empty.is_ok());
        let partial = plan_save(pg(), RoutineKind::View, "\"v\"", "SELECT 1;");
        assert!(!partial.is_ok());
        assert!(partial.error.unwrap().contains("CREATE"));
    }

    #[test]
    fn leading_comments_do_not_hide_the_create_keyword() {
        let plan = plan_save(
            pg(),
            RoutineKind::View,
            "\"v\"",
            "-- 由工具生成\nCREATE VIEW v AS SELECT 1;",
        );
        assert!(plan.is_ok());
        assert!(plan.statements[0].starts_with("-- 由工具生成"));
        assert!(plan.statements[0].contains("CREATE OR REPLACE VIEW"));
    }
}
