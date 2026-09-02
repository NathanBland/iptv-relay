// Print the OpenAPI document that the core service serves at
// /api/v1/openapi.json. Use this example to regenerate the checked-in
// reference snapshot without a database or secrets.
//
// Run: cargo run -p iptv-api --example print-openapi

use iptv_api::ApiDoc;
use utoipa::OpenApi;

fn main() {
    let doc = ApiDoc::openapi();
    let serialized = serde_json::to_string_pretty(&doc).expect("serialize openapi");
    println!("{serialized}");
}
