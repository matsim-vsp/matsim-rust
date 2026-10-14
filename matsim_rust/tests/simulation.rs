#[path = "simulation/accessibility_analysis.rs"]
mod accessibility_analysis;
#[path = "simulation/automatic_analysis.rs"]
mod automatic_analysis;
#[path = "simulation/berlin.rs"]
pub mod berlin;
#[path = "simulation/daily_patterns.rs"]
mod daily_patterns;
#[path = "simulation/demographic_analysis.rs"]
mod demographic_analysis;
#[path = "simulation/empty.rs"]
mod empty;
#[path = "simulation/equil.rs"]
mod equil;
#[path = "simulation/equil_teleport.rs"]
mod equil_teleport;
#[path = "simulation/iterations.rs"]
mod iterations;
#[path = "simulation/link_speed_analysis.rs"]
mod link_speed_analysis;
#[path = "simulation/pt.rs"]
mod pt;
#[path = "simulation/standalone_analysis.rs"]
mod standalone_analysis;
#[path = "simulation/three_links.rs"]
mod three_links;

fn visual_report_table(html: &str, file: &str) -> serde_json::Value {
    let (_, data) = html
        .split_once("<script id=\"report-data\" type=\"application/json\">")
        .unwrap();
    let (data, _) = data.split_once("</script>").unwrap();
    let data: serde_json::Value = serde_json::from_str(data).unwrap();
    data["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|table| table["file"] == file)
        .unwrap()
        .clone()
}
