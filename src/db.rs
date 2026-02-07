use std::{collections::HashMap, ops::Deref};

use sqlx::{Row, SqlitePool};

use crate::types::{
    LookupId, LookupName, Pattern, PatternQuery, Schema, SchemaQuery, TableName, WMIInfo,
};

pub struct Db {
    pool: SqlitePool,
}

impl Db {
    pub async fn new() -> Self {
        let url = std::env::var("DATABASE_URL").expect("database url env var missing");
        let pool = SqlitePool::connect(&url)
            .await
            .expect("sqlite connection failure");

        Self { pool }
    }

    pub(crate) async fn get_wmi_infos(&self, wmis: &[&str]) -> Result<Vec<WMIInfo>, sqlx::Error> {
        let values_clause = wmis.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            r#"
                WITH RECURSIVE
                      WmiMakes AS (
                        SELECT
                          w.Id as WmiId,
                          w.Wmi as code,
                          m.Name as manufacturer,
                          ma.Name as make,
                          c.Name as country,
                          vt.Name as vehicleType,
                          CASE
                            WHEN c.Name IN ('UNITED STATES', 'CANADA', 'MEXICO') THEN 'NORTH AMERICA'
                            WHEN c.Name IN ('JAPAN', 'KOREA', 'CHINA', 'TAIWAN') THEN 'ASIA'
                            WHEN c.Name IN ('GERMANY', 'UNITED KINGDOM', 'ITALY', 'FRANCE', 'SWEDEN') THEN 'EUROPE'
                            ELSE 'OTHER'
                          END as region,
                          ROW_NUMBER() OVER (PARTITION BY w.Wmi ORDER BY w.CreatedOn DESC
                          ) as rn
                        FROM Wmi w
                        LEFT JOIN Manufacturer m ON w.ManufacturerId = m.Id
                        LEFT JOIN Wmi_Make wm ON w.Id = wm.WmiId
                        LEFT JOIN Make ma ON wm.MakeId = ma.Id
                        LEFT JOIN Country c ON w.CountryId = c.Id
                        LEFT JOIN VehicleType vt ON w.VehicleTypeId = vt.Id
                        WHERE w.Wmi = {}
                      )
                      SELECT
                        code,
                        manufacturer,
                        make,
                        country,
                        vehicleType,
                        region
                      FROM WmiMakes
                      WHERE rn = 1
                "#,
            values_clause
        );

        let mut query = sqlx::query_as::<_, WMIInfo>(&sql);
        for w in wmis {
            query = query.bind(w);
        }

        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows)
    }

    pub(crate) async fn get_schemas(
        &self,
        schema_queries: Vec<SchemaQuery>,
    ) -> Result<Vec<Schema>, sqlx::Error> {
        let values_clause = schema_queries
            .iter()
            .map(|_| "(?, ?, ?, ?)")
            .collect::<Vec<_>>()
            .join(", ");

        let sql = format!(
            r#"
                WITH WmiYearPairs(Wmi, ModelYear, Vds, Vis) AS (
                    VALUES {}
                )
                SELECT vs.Id AS schema_id, vs.Name AS schema_name, wyp.Wmi AS wmi, wyp.ModelYear AS model_year, wyp.Vds as vds, wyp.Vis as vis
                FROM WmiYearPairs wyp
                JOIN Wmi w ON w.Wmi = wyp.Wmi
                JOIN Wmi_VinSchema wvs ON w.Id = wvs.WmiId
                JOIN VinSchema vs ON wvs.VinSchemaId = vs.Id
                WHERE wyp.ModelYear >= wvs.YearFrom
                AND (wvs.YearTo IS NULL OR wyp.ModelYear <= wvs.YearTo)
                GROUP BY vs.Id, vs.Name;
            "#,
            &values_clause
        );

        let mut query = sqlx::query_as::<_, Schema>(&sql);
        for q in schema_queries {
            query = query.bind(q.wmi).bind(q.model_year).bind(q.vds).bind(q.vis);
        }

        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows)
    }

    pub(crate) async fn get_patterns(
        &self,
        pattern_queries: Vec<PatternQuery>,
    ) -> Result<Vec<Pattern>, sqlx::Error> {
        let values_clause = pattern_queries
            .iter()
            .map(|_| "(?, ?, ?, ?, ?)")
            .collect::<Vec<_>>()
            .join(", ");

        let query = format!(
            r#"
                WITH Queries(SchemaId, Wmi, ModelYear, Vds, Vis) AS (
                    VALUES {}
                ),
                ValidSchemas AS (
                    SELECT vs.Id, vs.Name
                    FROM VinSchema vs
                    WHERE vs.Id IN (SELECT q.SchemaId FROM Queries q)
                )
                SELECT DISTINCT
                    p.Keys as Pattern,
                    e.Id as ElementId,
                    e.Name as ElementName,
                    e.Code as ElementCode,
                    e.GroupName,
                    e.Description,
                    e.LookupTable,
                    p.AttributeId,
                    vs.Name as SchemaName,
                    wvs.YearFrom,
                    wvs.YearTo,
                    e.weight as ElementWeight,
                    q.Wmi,
                    q.ModelYear,
                    q.Vds,
                    q.Vis
                FROM Pattern p
                JOIN Element e ON p.ElementId = e.Id
                JOIN ValidSchemas vs ON p.VinSchemaId = vs.Id
                JOIN Wmi_VinSchema wvs ON p.VinSchemaId = wvs.VinSchemaId
                JOIN Queries q ON p.VinSchemaId = q.SchemaId
                WHERE p.VinSchemaId IN (SELECT q.SchemaId FROM Queries q)

                UNION ALL

                SELECT
                    p.Keys as Pattern,
                    (SELECT Id FROM Element WHERE Name = 'Make' LIMIT 1) as ElementId,
                    'Make' as ElementName,
                    'MK' as ElementCode,
                    'Vehicle' as GroupName,
                    NULL as Description,
                    NULL as LookupTable,
                    m.Name as AttributeId,
                    vs.Name as SchemaName,
                    wvs.YearFrom,
                    wvs.YearTo,
                    (SELECT weight FROM Element WHERE Name = 'Make' LIMIT 1) as ElementWeight,
                    q.Wmi,
                    q.ModelYear,
                    q.Vds,
                    q.Vis
                FROM Pattern p
                JOIN Element e ON p.ElementId = e.Id
                JOIN ValidSchemas vs ON p.VinSchemaId = vs.Id
                JOIN Wmi_VinSchema wvs ON p.VinSchemaId = wvs.VinSchemaId
                JOIN Make_Model mm ON mm.ModelId = CAST(p.AttributeId AS INTEGER)
                JOIN Make m ON m.Id = mm.MakeId
                JOIN Queries q ON p.VinSchemaId = q.SchemaId
                WHERE e.Name = 'Model'
                AND p.VinSchemaId IN (SELECT q.SchemaId FROM Queries q)
            "#,
            values_clause
        );

        let mut query = sqlx::query_as::<_, Pattern>(&query);
        for q in pattern_queries {
            query = query
                .bind(q.schema_id)
                .bind(q.wmi)
                .bind(q.model_year)
                .bind(q.vds)
                .bind(q.vis);
        }

        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows)
    }

    pub(crate) async fn get_lookup(
        &self,
        table_ids: HashMap<TableName, Vec<LookupId>>,
    ) -> Result<HashMap<TableName, HashMap<LookupId, LookupName>>, sqlx::Error> {
        if table_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut sql_parts = Vec::new();
        let mut bindings: Vec<String> = Vec::new();

        for (table, ids) in table_ids {
            if table.is_empty() || ids.is_empty() {
                continue;
            }

            let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");

            sql_parts.push(format!(
                r#"
                    SELECT '{}' AS TableName,
                            CAST(Id AS TEXT) AS Id,
                            Name
                    FROM {}
                    WHERE CAST(Id AS TEXT) IN ({})
                "#,
                table, table, placeholders
            ));

            bindings.extend(ids);
        }

        if sql_parts.is_empty() {
            return Ok(HashMap::new());
        }

        let sql = sql_parts.join("\nUNION ALL\n");

        let mut query = sqlx::query(&sql);
        for value in bindings {
            query = query.bind(value);
        }

        let rows = query.fetch_all(&self.pool).await?;
        let mut result: HashMap<String, HashMap<String, String>> = HashMap::new();
        for row in rows {
            let table: String = row.get("TableName");
            let id: String = row.get("Id");
            let name: String = row.get("Name");

            result.entry(table).or_default().insert(id, name);
        }
        Ok(result)
    }
}

impl Deref for Db {
    type Target = SqlitePool;

    fn deref(&self) -> &Self::Target {
        &self.pool
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn test_db_env() {
        let url = std::env::var("DATABASE_URL");
        assert!(url.is_ok())
    }

    #[tokio::test]
    async fn test_db_establish_connection() {
        let db = Db::new().await;
        let conn = db.acquire().await;
        assert!(conn.is_ok())
    }

    #[tokio::test]
    async fn test_db_wmi() {
        let db = Db::new().await;
        let wmi_info = db.get_wmi_infos(&["KM8"]).await;
        assert!(wmi_info.is_ok())
    }

    #[tokio::test]
    async fn test_db_schema_one_element() {
        let db = Db::new().await;
        let schemas = db
            .get_schemas(vec![SchemaQuery {
                wmi: "KM8".to_string(),
                model_year: 2020,
                vds: "".to_string(),
                vis: "".to_string(),
            }])
            .await;
        assert!(schemas.is_ok_and(|inner| inner.len() > 0))
    }

    #[tokio::test]
    async fn test_db_schema_two_elements() -> Result<(), Box<dyn Error>> {
        let db = Db::new().await;
        let mut schemas_hyundai = db
            .get_schemas(vec![SchemaQuery {
                wmi: "KM8".to_string(),
                model_year: 2020,
                vds: "".to_string(),
                vis: "".to_string(),
            }])
            .await?;
        let schemas_toyota = db
            .get_schemas(vec![SchemaQuery {
                wmi: "4T1".to_string(),
                model_year: 2015,
                vds: "".to_string(),
                vis: "".to_string(),
            }])
            .await?;

        schemas_hyundai.extend(schemas_toyota);

        let schemas_both = db
            .get_schemas(vec![
                SchemaQuery {
                    wmi: "KM8".to_string(),
                    model_year: 2020,
                    vds: "".to_string(),
                    vis: "".to_string(),
                },
                SchemaQuery {
                    wmi: "4T1".to_string(),
                    model_year: 2015,
                    vds: "".to_string(),
                    vis: "".to_string(),
                },
            ])
            .await?;

        assert!(schemas_hyundai.len() == schemas_both.len());
        Ok(())
    }

    #[tokio::test]
    async fn test_db_schema_two_elements_overlapping() -> Result<(), Box<dyn Error>> {
        let db = Db::new().await;
        let mut schemas_hyundai_1 = db
            .get_schemas(vec![SchemaQuery {
                wmi: "KM8".to_string(),
                model_year: 2020,
                vds: "1".to_string(),
                vis: "1".to_string(),
            }])
            .await?;
        let schemas_hyundai_2 = db
            .get_schemas(vec![SchemaQuery {
                wmi: "KM8".to_string(),
                model_year: 2019,
                vds: "1".to_string(),
                vis: "1".to_string(),
            }])
            .await?;
        schemas_hyundai_1.extend(schemas_hyundai_2);

        let schemas_both = db
            .get_schemas(vec![
                SchemaQuery {
                    wmi: "KM8".to_string(),
                    model_year: 2020,
                    vds: "1".to_string(),
                    vis: "1".to_string(),
                },
                SchemaQuery {
                    wmi: "KM8".to_string(),
                    model_year: 2019,
                    vds: "1".to_string(),
                    vis: "1".to_string(),
                },
            ])
            .await?;

        assert!(schemas_hyundai_1.len() != schemas_both.len());
        Ok(())
    }

    #[tokio::test]
    async fn test_db_pattern() -> Result<(), Box<dyn Error>> {
        let db = Db::new().await;
        let patterns = db
            .get_patterns(vec![PatternQuery {
                schema_id: 20428,
                wmi: "".to_string(),
                model_year: 0,
                vds: "".to_string(),
                vis: "".to_string(),
            }])
            .await?;

        assert!(patterns.len() == 11);
        Ok(())
    }

    #[tokio::test]
    async fn test_db_lookup() -> Result<(), Box<dyn Error>> {
        let db = Db::new().await;
        let mut map = HashMap::new();
        map.insert("FuelType".to_string(), vec!["18".to_string()]);
        map.insert("BodyStyle".to_string(), vec!["7".to_string()]);
        let lookup = db.get_lookup(map).await?;

        assert!(lookup.len() == 2);
        Ok(())
    }
}
