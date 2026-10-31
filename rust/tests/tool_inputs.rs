use reagent::tools::{execute, ToolState};
use serde_json::json;

#[test]
fn invalid_unicode_regex_returns_an_error_and_the_next_search_works() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("text"), "ordinary text\n").unwrap();
    let config = json!({"workspace":directory.path()});
    let mut state = ToolState::new(&config).unwrap();
    for query in ["\\é", "[\\界]"] {
        let args = json!({"path":".","query":query,"regex":true}).to_string();
        let result = execute(&mut state, &config, "search_text", &args).unwrap();
        assert_eq!(result["code"], "invalid_arguments", "{result}");
    }
    let result = execute(
        &mut state,
        &config,
        "search_text",
        r#"{"path":".","query":"ordinary"}"#,
    )
    .unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["data"]["matches"][0]["line"], 1);
}
