use rustc_hash::FxHashSet;
use squawk_syntax::{
    Parse, SourceFile, SyntaxNode,
    ast::{self, AstNode, NameLike},
};

use crate::{Linter, Rule, Violation};

pub(crate) fn require_query_table_schema(ctx: &mut Linter, parse: &Parse<SourceFile>) {
    let file = parse.tree();
    let temp_tables = temp_table_names(&file);
    for stmt in file.stmts() {
        match stmt {
            ast::Stmt::Select(_)
            | ast::Stmt::CompoundSelect(_)
            | ast::Stmt::ParenSelect(_)
            | ast::Stmt::Insert(_)
            | ast::Stmt::Update(_)
            | ast::Stmt::Delete(_) => check_query(ctx, stmt.syntax(), &temp_tables),
            _ => (),
        }
    }
}

/// Temp tables are exempt from schema qualification, so we need their names to
/// avoid warning when a later query refers to one.
fn temp_table_names(file: &SourceFile) -> FxHashSet<String> {
    let mut names = FxHashSet::default();
    for stmt in file.stmts() {
        let table_name = match stmt {
            ast::Stmt::CreateTable(create_table) => {
                if !matches!(create_table.persistence(), Some(ast::Persistence::Temp(_))) {
                    continue;
                }
                create_table.table_name()
            }
            ast::Stmt::CreateTableAs(create_table_as) => {
                if !matches!(
                    create_table_as.persistence(),
                    Some(ast::Persistence::Temp(_))
                ) {
                    continue;
                }
                create_table_as.table_name()
            }
            _ => continue,
        };
        if let Some(segment) = table_name.and_then(|table| table.path()?.segment()) {
            names.insert(segment.text());
        }
    }
    names
}

/// Check every table referenced by a query, including those in sub queries,
/// e.g., the `FROM` of an `UPDATE`, or a `SELECT` inside a `WHERE`.
fn check_query(ctx: &mut Linter, stmt: &SyntaxNode, temp_tables: &FxHashSet<String>) {
    for relation in stmt.descendants().filter_map(ast::RelationNameRef::cast) {
        let Some(path) = relation.path_ref() else {
            continue;
        };
        if path.qualifier().is_some() {
            continue;
        }
        // CTEs and temp tables are only ever referred to by their bare name.
        if let Some(segment) = path.segment()
            && (resolves_to_cte(&relation, &segment) || temp_tables.contains(&segment.text()))
        {
            continue;
        }
        ctx.report(Violation::for_node(
            Rule::RequireQueryTableSchema,
            "Table name is not schema-qualified. Use schema.table (e.g., public.my_table).".into(),
            path.syntax(),
        ));
    }
}

fn resolves_to_cte(relation: &ast::RelationNameRef, segment: &ast::PathSegmentRef) -> bool {
    let name = segment.text();
    let relation_start = relation.syntax().text_range().start();

    relation
        .syntax()
        .ancestors()
        .filter_map(|query| ast::WithQuery::cast(query)?.with_clause())
        .any(|with_clause| {
            let is_recursive = with_clause.recursive_token().is_some();
            with_clause
                .with_tables()
                // Without RECURSIVE, only CTEs declared before the reference are visible.
                .filter(|with_table| {
                    is_recursive || with_table.syntax().text_range().end() <= relation_start
                })
                .any(|with_table| with_table.name().is_some_and(|it| it.text() == name))
        })
}

#[cfg(test)]
mod test {
    use insta::assert_snapshot;

    use crate::Rule;
    use crate::test_utils::{lint_errors, lint_ok};

    #[test]
    fn select_err() {
        let sql = r#"
SELECT * FROM my_table;
"#;
        assert_snapshot!(lint_errors(sql, Rule::RequireQueryTableSchema));
    }

    #[test]
    fn select_ok() {
        let sql = r#"
SELECT * FROM public.my_table;
"#;
        lint_ok(sql, Rule::RequireQueryTableSchema);
    }

    #[test]
    fn select_join_err() {
        let sql = r#"
SELECT *
FROM public.my_table
JOIN other_table ON other_table.id = public.my_table.other_id;
"#;
        assert_snapshot!(lint_errors(sql, Rule::RequireQueryTableSchema));
    }

    #[test]
    fn select_sub_query_err() {
        let sql = r#"
SELECT *
FROM public.my_table
WHERE id IN (SELECT id FROM other_table);
"#;
        assert_snapshot!(lint_errors(sql, Rule::RequireQueryTableSchema));
    }

    #[test]
    fn compound_select_err() {
        let sql = r#"
SELECT id FROM public.my_table
UNION ALL
SELECT id FROM other_table;
"#;
        assert_snapshot!(lint_errors(sql, Rule::RequireQueryTableSchema));
    }

    #[test]
    fn select_without_from_ok() {
        let sql = r#"
SELECT 1;
"#;
        lint_ok(sql, Rule::RequireQueryTableSchema);
    }

    #[test]
    fn select_from_function_ok() {
        let sql = r#"
SELECT * FROM generate_series(1, 10);
"#;
        lint_ok(sql, Rule::RequireQueryTableSchema);
    }

    #[test]
    fn select_cte_ok() {
        let sql = r#"
WITH recent AS (
    SELECT * FROM public.my_table
)
SELECT * FROM recent;
"#;
        lint_ok(sql, Rule::RequireQueryTableSchema);
    }

    #[test]
    fn select_cte_body_err() {
        let sql = r#"
WITH recent AS (
    SELECT * FROM my_table
)
SELECT * FROM recent;
"#;
        assert_snapshot!(lint_errors(sql, Rule::RequireQueryTableSchema));
    }

    #[test]
    fn select_recursive_cte_ok() {
        let sql = r#"
WITH RECURSIVE tree AS (
    SELECT id, parent_id FROM public.my_table WHERE parent_id IS NULL
    UNION ALL
    SELECT child.id, child.parent_id
    FROM public.my_table AS child
    JOIN tree ON tree.id = child.parent_id
)
SELECT * FROM tree;
"#;
        lint_ok(sql, Rule::RequireQueryTableSchema);
    }

    #[test]
    fn select_from_temp_table_ok() {
        let sql = r#"
CREATE TEMP TABLE temp_mapping_table (id int4) ON COMMIT DROP;
SELECT * FROM temp_mapping_table;
INSERT INTO public.my_table (id) SELECT id FROM temp_mapping_table;
"#;
        lint_ok(sql, Rule::RequireQueryTableSchema);
    }

    #[test]
    fn insert_err() {
        let sql = r#"
INSERT INTO my_table (id) VALUES (1);
"#;
        assert_snapshot!(lint_errors(sql, Rule::RequireQueryTableSchema));
    }

    #[test]
    fn insert_ok() {
        let sql = r#"
INSERT INTO public.my_table (id) VALUES (1);
"#;
        lint_ok(sql, Rule::RequireQueryTableSchema);
    }

    #[test]
    fn insert_from_select_err() {
        let sql = r#"
INSERT INTO public.my_table (id) SELECT id FROM other_table;
"#;
        assert_snapshot!(lint_errors(sql, Rule::RequireQueryTableSchema));
    }

    #[test]
    fn update_err() {
        let sql = r#"
UPDATE my_table SET id = 1;
"#;
        assert_snapshot!(lint_errors(sql, Rule::RequireQueryTableSchema));
    }

    #[test]
    fn update_ok() {
        let sql = r#"
UPDATE public.my_table SET id = 1;
"#;
        lint_ok(sql, Rule::RequireQueryTableSchema);
    }

    #[test]
    fn update_from_err() {
        let sql = r#"
UPDATE public.my_table
SET id = other_table.id
FROM other_table
WHERE other_table.key = public.my_table.key;
"#;
        assert_snapshot!(lint_errors(sql, Rule::RequireQueryTableSchema));
    }

    #[test]
    fn delete_err() {
        let sql = r#"
DELETE FROM my_table WHERE id = 1;
"#;
        assert_snapshot!(lint_errors(sql, Rule::RequireQueryTableSchema));
    }

    #[test]
    fn delete_ok() {
        let sql = r#"
DELETE FROM public.my_table WHERE id = 1;
"#;
        lint_ok(sql, Rule::RequireQueryTableSchema);
    }

    #[test]
    fn delete_using_err() {
        let sql = r#"
DELETE FROM public.my_table
USING other_table
WHERE other_table.id = public.my_table.id;
"#;
        assert_snapshot!(lint_errors(sql, Rule::RequireQueryTableSchema));
    }
    #[test]
    fn select_quoted_cte_ok() {
        let sql = r#"
WITH "Recent" AS (
    SELECT * FROM public.my_table
)
SELECT * FROM "Recent";
"#;
        lint_ok(sql, Rule::RequireQueryTableSchema);
    }

    #[test]
    fn select_comma_join_err() {
        let sql = r#"
SELECT * FROM public.my_table, other_table;
"#;
        assert_snapshot!(lint_errors(sql, Rule::RequireQueryTableSchema));
    }

    #[test]
    fn select_cte_out_of_scope_err() {
        let sql = r#"
SELECT * FROM (
    WITH inner_cte AS (SELECT 1 AS id)
    SELECT * FROM inner_cte
) AS sub
JOIN inner_cte ON inner_cte.id = sub.id;
"#;
        assert_snapshot!(lint_errors(sql, Rule::RequireQueryTableSchema));
    }
}
