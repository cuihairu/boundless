//! Entity schema updates: bringing an existing table up to the current
//! definition (`server/tools/sync_db/mysql_synchronise.cpp`
//! `syncAllEntityTables`, which creates missing tables and alters
//! changed ones).
//!
//! An entity definition grows over time: a property is added, the table
//! gains a column. The rows written by the older definition must stay
//! readable — the update adds the column `NOT NULL DEFAULT <neutral>`,
//! so every existing row reads back with the neutral value for the new
//! property (the unset fixed dict for a nested column, zero for a
//! counter) and the record's other columns untouched.

use db::DbError;
use postgres::Client;

use crate::schema::{ColumnKind, EntitySchema, column_name, entity_table_name, quote_ident};
use crate::store::map_db_error;

/// The neutral value a new column gives the rows that predate it: the
/// unset shape for its kind.
pub fn column_default(kind: ColumnKind) -> &'static str {
    match kind {
        ColumnKind::Int | ColumnKind::Uint => "0",
        ColumnKind::Float => "0.0",
        ColumnKind::String => "''",
        ColumnKind::Blob => "''::BYTEA",
        ColumnKind::Vector => "ARRAY[]::REAL[]",
        // The unset fixed dict is the neutral nested value.
        ColumnKind::Nested => "'{\"d\":null}'::JSONB",
    }
}

/// The update plan for one entity table: create the table when it does
/// not exist, otherwise one `ALTER TABLE ... ADD COLUMN ... NOT NULL
/// DEFAULT ...` per property the table lacks. Existing tables are never
/// dropped or rewritten.
pub fn plan_schema_update(
    client: &mut Client,
    schema: &EntitySchema,
) -> Result<Vec<String>, DbError> {
    let table = entity_table_name(&schema.type_name);
    let exists = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM pg_tables WHERE schemaname = 'public' AND tablename = $1)",
            &[&table],
        )
        .map_err(map_db_error)?
        .get::<_, bool>(0);
    if !exists {
        return Ok(vec![schema.create_table_ddl()]);
    }

    let actual: Vec<String> = client
        .query(
            "SELECT column_name FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = $1 ORDER BY column_name",
            &[&table],
        )
        .map_err(map_db_error)?
        .iter()
        .map(|row| row.get(0))
        .collect();

    let mut plan = Vec::new();
    for (prop, kind) in &schema.columns {
        let column = column_name(prop);
        if !actual.contains(&column) {
            plan.push(format!(
                "ALTER TABLE {} ADD COLUMN {} {} NOT NULL DEFAULT {}",
                quote_ident(&table),
                quote_ident(&column),
                kind.sql_type(),
                column_default(*kind)
            ));
        }
    }
    Ok(plan)
}

/// Applies an update plan, in order.
pub fn apply_schema_update(client: &mut Client, plan: &[String]) -> Result<(), DbError> {
    for statement in plan {
        client
            .execute(statement.as_str(), &[])
            .map_err(map_db_error)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use entitydef::value::PropertyValue;

    fn schema_with(props: &[(&str, PropertyValue)]) -> EntitySchema {
        let names: Vec<&str> = props.iter().map(|(name, _)| *name).collect();
        let values: Vec<PropertyValue> = props.iter().map(|(_, value)| value.clone()).collect();
        EntitySchema::for_record("Account", &names, &values).unwrap()
    }

    #[test]
    fn every_kind_has_an_unset_default() {
        assert_eq!(column_default(ColumnKind::Int), "0");
        assert_eq!(column_default(ColumnKind::Uint), "0");
        assert_eq!(column_default(ColumnKind::Float), "0.0");
        assert_eq!(column_default(ColumnKind::String), "''");
        assert_eq!(column_default(ColumnKind::Blob), "''::BYTEA");
        assert_eq!(column_default(ColumnKind::Vector), "ARRAY[]::REAL[]");
        assert_eq!(column_default(ColumnKind::Nested), "'{\"d\":null}'::JSONB");
    }

    #[test]
    fn the_update_statement_carries_the_column_and_default() {
        let schema = schema_with(&[
            ("name", PropertyValue::String("x".into())),
            ("level", PropertyValue::Int(1)),
            ("loot", PropertyValue::Sequence(vec![])),
            ("pos", PropertyValue::Vector(vec![1.0])),
        ]);
        // A table that only has the first column gains the other three.
        let actual = ["id".to_string(), "sm_name".to_string()];
        let plan: Vec<String> = schema
            .columns
            .iter()
            .filter(|(prop, _)| !actual.contains(&column_name(prop)))
            .map(|(prop, kind)| {
                format!(
                    "ALTER TABLE {} ADD COLUMN {} {} NOT NULL DEFAULT {}",
                    quote_ident(&entity_table_name(&schema.type_name)),
                    quote_ident(&column_name(prop)),
                    kind.sql_type(),
                    column_default(*kind)
                )
            })
            .collect();
        assert_eq!(
            plan,
            vec![
                "ALTER TABLE \"tbl_Account\" ADD COLUMN \"sm_level\" BIGINT NOT NULL DEFAULT 0",
                "ALTER TABLE \"tbl_Account\" ADD COLUMN \"sm_loot\" JSONB NOT NULL DEFAULT '{\"d\":null}'::JSONB",
                "ALTER TABLE \"tbl_Account\" ADD COLUMN \"sm_pos\" REAL[] NOT NULL DEFAULT ARRAY[]::REAL[]",
            ]
        );
    }
}
