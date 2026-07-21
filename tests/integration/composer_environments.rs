use crate::common::create_test_backend;
use std::fs;
use std::path::Path;
use tower_lsp::LanguageServer;
use tower_lsp::lsp_types::*;

const FORM_SOURCE: &str = concat!(
    "<?php\n",
    "namespace FormApp;\n",
    "use Shared\\ArrayUtils;\n",
    "final class Consumer {\n",
    "    public function run(ArrayUtils $utils): void {}\n",
    "}\n",
);

const HTTP_SOURCE: &str = concat!(
    "<?php\n",
    "namespace HttpApp;\n",
    "use Shared\\ArrayUtils;\n",
    "final class Consumer {\n",
    "    public function run(ArrayUtils $utils): void {}\n",
    "}\n",
);

fn write_project(workspace: &Path, directory: &str, namespace: &str, source: &str) {
    let root = workspace.join(directory);
    let vendor = root.join("vendor");
    let package = vendor.join("acme/shared");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(package.join("src")).unwrap();
    fs::create_dir_all(vendor.join("composer")).unwrap();

    fs::write(
        root.join("composer.json"),
        format!(
            r#"{{"name":"app/{directory}","require":{{"acme/shared":"*"}},"autoload":{{"psr-4":{{"{namespace}\\\\":"src/"}}}}}}"#,
        ),
    )
    .unwrap();
    fs::write(root.join("src/Consumer.php"), source).unwrap();
    fs::write(
        vendor.join("composer/installed.json"),
        r#"{"packages":[{"name":"acme/shared","install-path":"../acme/shared","autoload":{"psr-4":{"Shared\\":"src/"}}}]}"#,
    )
    .unwrap();

    // Deliberately use different declaration lines. A wrong URI or ClassInfo
    // is therefore visible in both the target file and target position.
    let leading_lines = if directory == "form" { 1 } else { 4 };
    let mut class_source = String::from("<?php\nnamespace Shared;\n");
    for _ in 0..leading_lines {
        class_source.push('\n');
    }
    class_source.push_str("abstract class ArrayUtils {}\n");
    fs::write(package.join("src/ArrayUtils.php"), class_source).unwrap();
}

fn position_of_nth(content: &str, needle: &str, occurrence: usize) -> Position {
    let offset = content
        .match_indices(needle)
        .nth(occurrence)
        .map(|(offset, _)| offset)
        .expect("needle occurrence should exist");
    let before = &content[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let character = before
        .rsplit_once('\n')
        .map_or(before.len(), |(_, tail)| tail.len()) as u32;
    Position { line, character }
}

async fn open(backend: &phpantom_lsp::Backend, path: &Path) -> (Url, String) {
    let uri = Url::from_file_path(path).unwrap();
    let text = fs::read_to_string(path).unwrap();
    backend
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "php".to_string(),
                version: 1,
                text: text.clone(),
            },
        })
        .await;
    (uri, text)
}

async fn definition_location(
    backend: &phpantom_lsp::Backend,
    uri: Url,
    position: Position,
) -> Location {
    let response = backend
        .goto_definition(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .unwrap()
        .expect("class definition should resolve");

    match response {
        GotoDefinitionResponse::Scalar(location) => location,
        other => panic!("expected one class definition, got {other:?}"),
    }
}

async fn run_duplicate_vendor_case(reverse_vendor_open_order: Option<bool>) {
    let workspace = tempfile::tempdir().unwrap();
    write_project(workspace.path(), "form", "FormApp", FORM_SOURCE);
    write_project(workspace.path(), "http", "HttpApp", HTTP_SOURCE);

    let backend = create_test_backend();
    backend
        .initialize(InitializeParams {
            root_uri: Some(Url::from_directory_path(workspace.path()).unwrap()),
            ..InitializeParams::default()
        })
        .await
        .unwrap();
    backend.initialized(InitializedParams {}).await;

    let form_root = workspace.path().join("form");
    let http_root = workspace.path().join("http");
    let form_vendor = form_root.join("vendor/acme/shared/src/ArrayUtils.php");
    let http_vendor = http_root.join("vendor/acme/shared/src/ArrayUtils.php");
    if let Some(reverse_vendor_open_order) = reverse_vendor_open_order {
        if reverse_vendor_open_order {
            open(&backend, &http_vendor).await;
            open(&backend, &form_vendor).await;
        } else {
            open(&backend, &form_vendor).await;
            open(&backend, &http_vendor).await;
        }
    }

    let (form_uri, form_text) = open(&backend, &form_root.join("src/Consumer.php")).await;
    let (http_uri, http_text) = open(&backend, &http_root.join("src/Consumer.php")).await;
    let form_position = position_of_nth(&form_text, "ArrayUtils", 1);
    let http_position = position_of_nth(&http_text, "ArrayUtils", 1);

    let form_definition = definition_location(&backend, form_uri.clone(), form_position).await;
    let http_definition = definition_location(&backend, http_uri.clone(), http_position).await;
    assert_eq!(
        form_definition.uri.to_file_path().unwrap(),
        form_vendor,
        "Form must resolve its own Composer environment"
    );
    assert_eq!(
        http_definition.uri.to_file_path().unwrap(),
        http_vendor,
        "HTTP must resolve its own Composer environment"
    );
    assert_eq!(form_definition.range.start.line, 3);
    assert_eq!(http_definition.range.start.line, 6);

    let form_references = backend
        .find_references(form_uri.as_str(), &form_text, form_position, true)
        .expect("Form references should resolve");
    let http_references = backend
        .find_references(http_uri.as_str(), &http_text, http_position, true)
        .expect("HTTP references should resolve");

    assert!(!form_references.is_empty());
    assert!(!http_references.is_empty());
    assert_locations_belong_to(&form_references, &form_root, &http_root);
    assert_locations_belong_to(&http_references, &http_root, &form_root);

    // When both physical vendor declarations are open, invoking references
    // directly on either declaration must retain the same environment scope.
    if reverse_vendor_open_order.is_some() {
        let form_vendor_text = fs::read_to_string(&form_vendor).unwrap();
        let http_vendor_text = fs::read_to_string(&http_vendor).unwrap();
        let form_vendor_references = backend
            .find_references(
                Url::from_file_path(&form_vendor).unwrap().as_str(),
                &form_vendor_text,
                position_of_nth(&form_vendor_text, "ArrayUtils", 0),
                true,
            )
            .expect("Form vendor declaration references should resolve");
        let http_vendor_references = backend
            .find_references(
                Url::from_file_path(&http_vendor).unwrap().as_str(),
                &http_vendor_text,
                position_of_nth(&http_vendor_text, "ArrayUtils", 0),
                true,
            )
            .expect("HTTP vendor declaration references should resolve");
        assert_eq!(form_vendor_references, form_references);
        assert_eq!(http_vendor_references, http_references);
    }
}

fn assert_locations_belong_to(locations: &[Location], expected: &Path, excluded: &Path) {
    for location in locations {
        let path = location.uri.to_file_path().unwrap();
        assert!(
            path.starts_with(expected),
            "reference {} escaped its Composer environment {}",
            path.display(),
            expected.display()
        );
        assert!(
            !path.starts_with(excluded),
            "reference {} leaked from Composer environment {}",
            path.display(),
            excluded.display()
        );
    }
}

#[tokio::test]
async fn duplicate_vendor_fqns_are_isolated_in_all_load_orders() {
    run_duplicate_vendor_case(None).await;
    run_duplicate_vendor_case(Some(false)).await;
    run_duplicate_vendor_case(Some(true)).await;
}
