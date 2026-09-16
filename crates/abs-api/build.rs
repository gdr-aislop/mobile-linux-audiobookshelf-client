use std::{env, fs, path::Path};

fn main() {
    let spec_path = "../../third_party/audiobookshelf-openapi/openapi.json";
    println!("cargo:rerun-if-changed={spec_path}");

    let src = fs::read_to_string(spec_path).expect("read vendored openapi spec");
    let spec: openapiv3::OpenAPI = serde_json::from_str(&src).expect("parse openapi spec");

    let mut generator = progenitor::Generator::default();
    let tokens = generator
        .generate_tokens(&spec)
        .expect("generate client from openapi spec");
    let ast: syn::File = syn::parse2(tokens).expect("parse generated tokens as a syn::File");
    let content = prettyplease::unparse(&ast);

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    fs::write(Path::new(&out_dir).join("client.rs"), content).expect("write generated client.rs");
}
