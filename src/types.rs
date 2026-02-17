use sqlx::prelude::FromRow;

pub type TableName = String;
pub type LookupId = String;
pub type LookupName = String;

#[derive(FromRow, Debug, PartialEq, Clone)]
pub struct WMIInfo {
    pub code: String,

    pub manufacturer: String,

    pub country: String,

    #[sqlx(rename = "vehicleType")]
    pub vehicle_type: String,

    pub region: String,

    pub make: String,
}

pub struct SchemaQuery {
    pub wmi: String,
    pub vds: String,
    pub vis: String,
    pub model_year: i32,
}

#[derive(FromRow, Debug, PartialEq)]
pub struct Schema {
    pub schema_id: i32,
    pub schema_name: String,
    pub wmi: String,
    pub vds: String,
    pub vis: String,
    pub model_year: i32,
}

pub struct PatternQuery {
    pub schema_id: i32,
    pub wmi: String,
    pub vds: String,
    pub vis: String,
    pub model_year: i32,
}

#[derive(sqlx::FromRow, Debug, Clone)]
pub struct Pattern {
    #[sqlx(rename = "Pattern")]
    pub pattern: String,
    #[sqlx(rename = "ElementId")]
    pub element_id: i64,
    #[sqlx(rename = "ElementName")]
    pub element_name: String,
    #[sqlx(rename = "ElementCode")]
    pub element_code: String,
    #[sqlx(rename = "GroupName")]
    pub group_name: Option<String>,
    #[sqlx(rename = "Description")]
    pub description: Option<String>,
    #[sqlx(rename = "LookupTable")]
    pub lookup_table: Option<String>,
    #[sqlx(rename = "AttributeId")]
    pub attribute_id: String,
    #[sqlx(rename = "SchemaName")]
    pub schema_name: String,
    #[sqlx(rename = "YearFrom")]
    pub year_from: Option<i32>,
    #[sqlx(rename = "YearTo")]
    pub year_to: Option<i32>,
    #[sqlx(rename = "ElementWeight")]
    pub element_weight: Option<i32>,
    #[sqlx(rename = "Wmi")]
    pub wmi: String,
    #[sqlx(rename = "Vds")]
    pub vds: String,
    #[sqlx(rename = "Vis")]
    pub vis: String,
    #[sqlx(rename = "ModelYear")]
    pub model_year: i32,
}

#[derive(Debug, Clone)]
pub struct ResolvedPattern {
    pub pattern: Pattern,
    pub resolved: String,
}

#[derive(Debug)]
pub enum PatternType {
    VDS,
    VIS,
}

#[derive(Debug)]
pub struct RawPattern {
    pub pattern: Pattern,
    pub resolved: String,
    pub confidence: f64,
    pub positions: Vec<usize>,
    pub pattern_type: PatternType,
}
