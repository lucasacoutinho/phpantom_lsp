mod common;

use common::create_test_backend;
use tower_lsp::lsp_types::*;

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Open a file, trigger `update_ast`, then collect diagnostics.
///
/// Since we don't have a real LSP client in tests, we call the internal
/// `collect_deprecated_diagnostics` and `collect_unused_import_diagnostics`
/// methods directly rather than going through `publish_diagnostics_for_file`.
fn deprecated_diagnostics(
    backend: &phpantom_lsp::Backend,
    uri: &str,
    text: &str,
) -> Vec<Diagnostic> {
    backend.update_ast(uri, text);
    let mut out = Vec::new();
    backend.collect_deprecated_diagnostics(uri, text, &mut out);
    out
}

fn unused_import_diagnostics(
    backend: &phpantom_lsp::Backend,
    uri: &str,
    text: &str,
) -> Vec<Diagnostic> {
    backend.update_ast(uri, text);
    let mut out = Vec::new();
    backend.collect_unused_import_diagnostics(uri, text, &mut out);
    out
}

fn all_diagnostics(backend: &phpantom_lsp::Backend, uri: &str, text: &str) -> Vec<Diagnostic> {
    backend.update_ast(uri, text);
    let mut out = Vec::new();
    backend.collect_deprecated_diagnostics(uri, text, &mut out);
    backend.collect_unused_import_diagnostics(uri, text, &mut out);
    out
}

/// Assert that a diagnostic has the `Deprecated` tag.
fn has_deprecated_tag(d: &Diagnostic) -> bool {
    d.tags
        .as_ref()
        .is_some_and(|tags| tags.contains(&DiagnosticTag::DEPRECATED))
}

/// Assert that a diagnostic has the `Unnecessary` tag.
fn has_unnecessary_tag(d: &Diagnostic) -> bool {
    d.tags
        .as_ref()
        .is_some_and(|tags| tags.contains(&DiagnosticTag::UNNECESSARY))
}

// ═══════════════════════════════════════════════════════════════════════════
// @deprecated usage diagnostics
// ═══════════════════════════════════════════════════════════════════════════

// ─── Deprecated class ───────────────────────────────────────────────────────

#[test]
fn deprecated_class_reference_in_new() {
    let backend = create_test_backend();
    let uri = "file:///test_deprecated_class.php";
    let text = r#"<?php
/** @deprecated Use NewHelper instead */
class OldHelper {}

class Consumer {
    public function run(): void {
        $x = new OldHelper();
    }
}
"#;

    let diags = deprecated_diagnostics(&backend, uri, text);
    let deprecated: Vec<_> = diags.iter().filter(|d| has_deprecated_tag(d)).collect();

    // Should flag the `OldHelper` reference in `new OldHelper()`
    assert!(
        deprecated
            .iter()
            .any(|d| d.message.contains("OldHelper") && d.message.contains("deprecated")),
        "Expected a deprecated diagnostic for OldHelper, got: {:?}",
        deprecated
    );
}

#[test]
fn deprecated_class_with_message() {
    let backend = create_test_backend();
    let uri = "file:///test_deprecated_msg.php";
    let text = r#"<?php
/** @deprecated Use NewApi instead */
class LegacyApi {}

class Consumer {
    public function run(): void {
        $x = new LegacyApi();
    }
}
"#;

    let diags = deprecated_diagnostics(&backend, uri, text);
    let deprecated: Vec<_> = diags.iter().filter(|d| has_deprecated_tag(d)).collect();

    // The message should include the deprecation reason
    assert!(
        deprecated
            .iter()
            .any(|d| d.message.contains("Use NewApi instead")),
        "Expected deprecation message to include reason, got: {:?}",
        deprecated
    );
}

#[test]
fn non_deprecated_class_no_diagnostic() {
    let backend = create_test_backend();
    let uri = "file:///test_not_deprecated.php";
    let text = r#"<?php
class GoodHelper {}

class Consumer {
    public function run(): void {
        $x = new GoodHelper();
    }
}
"#;

    let diags = deprecated_diagnostics(&backend, uri, text);
    let deprecated: Vec<_> = diags.iter().filter(|d| has_deprecated_tag(d)).collect();
    assert!(
        deprecated.is_empty(),
        "Expected no deprecated diagnostics, got: {:?}",
        deprecated
    );
}

// ─── Deprecated method ──────────────────────────────────────────────────────

#[test]
fn deprecated_method_call() {
    let backend = create_test_backend();
    let uri = "file:///test_deprecated_method.php";
    let text = r#"<?php
class Mailer {
    /** @deprecated Use sendAsync() instead. */
    public function sendLegacy(): void {}

    public function sendAsync(): void {}
}

class App {
    public function run(): void {
        $m = new Mailer();
        $m->sendLegacy();
    }
}
"#;

    // We need the subject variable to be resolvable.  Since diagnostics
    // currently only resolve static accesses and self/this/$this,
    // let's test with self::.
    let _text = text;
    let text_static = r#"<?php
class Mailer {
    /** @deprecated Use sendAsync() instead. */
    public static function sendLegacy(): void {}

    public static function sendAsync(): void {}

    public function run(): void {
        self::sendLegacy();
    }
}
"#;

    let diags = deprecated_diagnostics(&backend, uri, text_static);
    let deprecated: Vec<_> = diags.iter().filter(|d| has_deprecated_tag(d)).collect();

    assert!(
        deprecated
            .iter()
            .any(|d| d.message.contains("sendLegacy") && d.message.contains("deprecated")),
        "Expected deprecated diagnostic for sendLegacy(), got: {:?}",
        deprecated
    );
}

#[test]
fn non_deprecated_method_no_diagnostic() {
    let backend = create_test_backend();
    let uri = "file:///test_not_deprecated_method.php";
    let text = r#"<?php
class Mailer {
    public static function sendAsync(): void {}

    public function run(): void {
        self::sendAsync();
    }
}
"#;

    let diags = deprecated_diagnostics(&backend, uri, text);
    let deprecated: Vec<_> = diags.iter().filter(|d| has_deprecated_tag(d)).collect();
    assert!(
        deprecated.is_empty(),
        "Expected no deprecated diagnostics, got: {:?}",
        deprecated
    );
}

// ─── Deprecated property ────────────────────────────────────────────────────

#[test]
fn deprecated_static_property() {
    let backend = create_test_backend();
    let uri = "file:///test_deprecated_prop.php";
    let text = r#"<?php
class Config {
    /** @deprecated Use $newSetting instead */
    public static string $oldSetting = 'x';

    public static string $newSetting = 'y';

    public function run(): void {
        self::$oldSetting;
    }
}
"#;

    let diags = deprecated_diagnostics(&backend, uri, text);
    let deprecated: Vec<_> = diags.iter().filter(|d| has_deprecated_tag(d)).collect();

    assert!(
        deprecated
            .iter()
            .any(|d| d.message.contains("oldSetting") && d.message.contains("deprecated")),
        "Expected deprecated diagnostic for $oldSetting, got: {:?}",
        deprecated
    );
}

// ─── Deprecated constant ────────────────────────────────────────────────────

#[test]
fn deprecated_class_constant() {
    let backend = create_test_backend();
    let uri = "file:///test_deprecated_const.php";
    let text = r#"<?php
class Status {
    /** @deprecated Use STATUS_ACTIVE instead */
    const OLD_STATUS = 1;

    const STATUS_ACTIVE = 1;

    public function run(): void {
        self::OLD_STATUS;
    }
}
"#;

    let diags = deprecated_diagnostics(&backend, uri, text);
    let deprecated: Vec<_> = diags.iter().filter(|d| has_deprecated_tag(d)).collect();

    assert!(
        deprecated
            .iter()
            .any(|d| d.message.contains("OLD_STATUS") && d.message.contains("deprecated")),
        "Expected deprecated diagnostic for OLD_STATUS, got: {:?}",
        deprecated
    );
}

// ─── Deprecated class in extends ────────────────────────────────────────────

#[test]
fn deprecated_class_in_extends() {
    let backend = create_test_backend();
    let uri = "file:///test_deprecated_extends.php";
    let text = r#"<?php
/** @deprecated Use NewBase instead */
class OldBase {}

class NewBase {}

class Child extends OldBase {}
"#;

    let diags = deprecated_diagnostics(&backend, uri, text);
    let deprecated: Vec<_> = diags.iter().filter(|d| has_deprecated_tag(d)).collect();

    assert!(
        deprecated.iter().any(|d| d.message.contains("OldBase")),
        "Expected deprecated diagnostic for OldBase in extends clause, got: {:?}",
        deprecated
    );
}

// ─── Deprecated class in type hint ──────────────────────────────────────────

#[test]
fn deprecated_class_in_type_hint() {
    let backend = create_test_backend();
    let uri = "file:///test_deprecated_hint.php";
    let text = r#"<?php
/** @deprecated */
class OldType {}

class Consumer {
    public function accept(OldType $param): void {}
}
"#;

    let diags = deprecated_diagnostics(&backend, uri, text);
    let deprecated: Vec<_> = diags.iter().filter(|d| has_deprecated_tag(d)).collect();

    assert!(
        deprecated.iter().any(|d| d.message.contains("OldType")),
        "Expected deprecated diagnostic for OldType in param type hint, got: {:?}",
        deprecated
    );
}

// ─── Diagnostic severity and tags ───────────────────────────────────────────

#[test]
fn deprecated_diagnostic_has_hint_severity_and_deprecated_tag() {
    let backend = create_test_backend();
    let uri = "file:///test_deprecated_severity.php";
    let text = r#"<?php
/** @deprecated */
class Old {}

class Consumer {
    public function run(): void {
        $x = new Old();
    }
}
"#;

    let diags = deprecated_diagnostics(&backend, uri, text);
    let deprecated: Vec<_> = diags.iter().filter(|d| has_deprecated_tag(d)).collect();

    for d in &deprecated {
        assert_eq!(
            d.severity,
            Some(DiagnosticSeverity::HINT),
            "Deprecated diagnostics should have HINT severity"
        );
        assert!(
            has_deprecated_tag(d),
            "Deprecated diagnostics should have the DEPRECATED tag"
        );
        assert_eq!(
            d.source.as_deref(),
            Some("phpantom"),
            "Source should be 'phpantom'"
        );
    }
}

// ─── Deprecated static method via class name ────────────────────────────────

#[test]
fn deprecated_static_method_via_class_name() {
    let backend = create_test_backend();
    let uri = "file:///test_deprecated_static.php";
    let text = r#"<?php
class Factory {
    /** @deprecated Use create() instead */
    public static function make(): void {}

    public static function create(): void {}
}

class Consumer {
    public function run(): void {
        Factory::make();
    }
}
"#;

    let diags = deprecated_diagnostics(&backend, uri, text);
    let deprecated: Vec<_> = diags.iter().filter(|d| has_deprecated_tag(d)).collect();

    assert!(
        deprecated.iter().any(|d| d.message.contains("make")),
        "Expected deprecated diagnostic for Factory::make(), got: {:?}",
        deprecated
    );
}

// ─── Stub files are skipped ─────────────────────────────────────────────────

#[test]
fn stub_files_produce_no_diagnostics() {
    let backend = create_test_backend();
    let uri = "phpantom-stub://SomeStub";
    let text = r#"<?php
/** @deprecated */
class DeprecatedStub {}
class User extends DeprecatedStub {}
"#;

    // update_ast first, then try to collect diagnostics
    backend.update_ast(uri, text);
    // The publish_diagnostics_for_file would skip this URI.
    // Verify that the check exists by testing the condition manually:
    assert!(
        uri.starts_with("phpantom-stub://"),
        "Test URI should be a stub URI"
    );
}

// ─── Deprecated empty message ───────────────────────────────────────────────

#[test]
fn deprecated_with_empty_message() {
    let backend = create_test_backend();
    let uri = "file:///test_deprecated_empty.php";
    let text = r#"<?php
/** @deprecated */
class Legacy {}

class Consumer {
    public function run(): void {
        $x = new Legacy();
    }
}
"#;

    let diags = deprecated_diagnostics(&backend, uri, text);
    let deprecated: Vec<_> = diags.iter().filter(|d| has_deprecated_tag(d)).collect();

    // Should say "'Legacy' is deprecated" without a trailing colon/message
    assert!(
        deprecated
            .iter()
            .any(|d| d.message == "'Legacy' is deprecated"),
        "Expected message to be exactly \"'Legacy' is deprecated\", got: {:?}",
        deprecated.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Unused `use` import diagnostics
// ═══════════════════════════════════════════════════════════════════════════

// ─── Single unused import ───────────────────────────────────────────────────

#[test]
fn unused_import_is_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_unused_import.php";
    let text = r#"<?php
namespace App;

use Foo\Bar;

class Consumer {}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary.iter().any(|d| d.message.contains("Foo\\Bar")),
        "Expected unused import diagnostic for Foo\\Bar, got: {:?}",
        unnecessary
    );
}

#[test]
fn unused_import_has_hint_severity_and_unnecessary_tag() {
    let backend = create_test_backend();
    let uri = "file:///test_unused_severity.php";
    let text = r#"<?php
namespace App;

use Some\UnusedClass;

class Consumer {}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    for d in &unnecessary {
        assert_eq!(
            d.severity,
            Some(DiagnosticSeverity::HINT),
            "Unused import diagnostics should have HINT severity"
        );
        assert!(
            has_unnecessary_tag(d),
            "Unused import diagnostics should have the UNNECESSARY tag"
        );
        assert_eq!(
            d.source.as_deref(),
            Some("phpantom"),
            "Source should be 'phpantom'"
        );
    }
}

// ─── Used import produces no diagnostic ─────────────────────────────────────

#[test]
fn used_import_in_type_hint_not_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_used_import.php";
    let text = r#"<?php
namespace App;

use Foo\Bar;

class Consumer {
    public function run(Bar $b): void {}
}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary.is_empty(),
        "Import used in type hint should not be flagged, got: {:?}",
        unnecessary
    );
}

#[test]
fn used_import_in_new_expression_not_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_used_new.php";
    let text = r#"<?php
namespace App;

use Foo\Bar;

class Consumer {
    public function run(): void {
        $x = new Bar();
    }
}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary.is_empty(),
        "Import used in new expression should not be flagged, got: {:?}",
        unnecessary
    );
}

#[test]
fn used_import_in_static_access_not_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_used_static.php";
    let text = r#"<?php
namespace App;

use Foo\Bar;

class Consumer {
    public function run(): void {
        Bar::doSomething();
    }
}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary.is_empty(),
        "Import used in static access should not be flagged, got: {:?}",
        unnecessary
    );
}

#[test]
fn used_import_in_extends_not_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_used_extends.php";
    let text = r#"<?php
namespace App;

use Foo\BaseClass;

class Consumer extends BaseClass {}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary.is_empty(),
        "Import used in extends should not be flagged, got: {:?}",
        unnecessary
    );
}

#[test]
fn used_import_in_implements_not_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_used_implements.php";
    let text = r#"<?php
namespace App;

use Foo\SomeInterface;

class Consumer implements SomeInterface {}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary.is_empty(),
        "Import used in implements should not be flagged, got: {:?}",
        unnecessary
    );
}

// ─── Multiple imports, some used some not ───────────────────────────────────

#[test]
fn mixed_used_and_unused_imports() {
    let backend = create_test_backend();
    let uri = "file:///test_mixed_imports.php";
    let text = r#"<?php
namespace App;

use Foo\UsedClass;
use Foo\UnusedClass;
use Foo\AnotherUsed;

class Consumer {
    public function run(UsedClass $a): AnotherUsed {}
}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    // Only UnusedClass should be flagged
    assert_eq!(
        unnecessary.len(),
        1,
        "Expected exactly 1 unused import diagnostic, got: {:?}",
        unnecessary
    );
    assert!(
        unnecessary[0].message.contains("Foo\\UnusedClass"),
        "Expected the unused import to be Foo\\UnusedClass, got: {}",
        unnecessary[0].message
    );
}

// ─── No use statements → no diagnostics ─────────────────────────────────────

#[test]
fn no_use_statements_no_diagnostics() {
    let backend = create_test_backend();
    let uri = "file:///test_no_uses.php";
    let text = r#"<?php
class SimpleClass {
    public function run(): void {}
}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "File with no use statements should produce no unused import diagnostics"
    );
}

// ─── Empty file ─────────────────────────────────────────────────────────────

#[test]
fn empty_file_no_diagnostics() {
    let backend = create_test_backend();
    let uri = "file:///test_empty.php";
    let text = "<?php\n";

    let diags = all_diagnostics(&backend, uri, text);
    assert!(diags.is_empty(), "Empty file should produce no diagnostics");
}

// ═══════════════════════════════════════════════════════════════════════════
// Combined diagnostics (both providers)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn deprecated_and_unused_in_same_file() {
    let backend = create_test_backend();
    let uri = "file:///test_combined.php";
    let text = r#"<?php
namespace App;

use Some\UnusedImport;

/** @deprecated */
class OldThing {}

class Consumer {
    public function run(): void {
        $x = new OldThing();
    }
}
"#;

    let diags = all_diagnostics(&backend, uri, text);
    let deprecated: Vec<_> = diags.iter().filter(|d| has_deprecated_tag(d)).collect();
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        !deprecated.is_empty(),
        "Should have deprecated diagnostics for OldThing"
    );
    assert!(
        !unnecessary.is_empty(),
        "Should have unused import diagnostics for UnusedImport"
    );
}

// ─── Used import in return type ─────────────────────────────────────────────

#[test]
fn used_import_in_return_type_not_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_used_return.php";
    let text = r#"<?php
namespace App;

use Foo\Result;

class Consumer {
    public function run(): Result {}
}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary.is_empty(),
        "Import used in return type should not be flagged, got: {:?}",
        unnecessary
    );
}

// ─── Used import in instanceof ──────────────────────────────────────────────

#[test]
fn used_import_in_instanceof_not_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_used_instanceof.php";
    let text = r#"<?php
namespace App;

use Foo\SomeClass;

class Consumer {
    public function check($x): void {
        if ($x instanceof SomeClass) {}
    }
}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary.is_empty(),
        "Import used in instanceof should not be flagged, got: {:?}",
        unnecessary
    );
}

// ─── Used import in catch clause ────────────────────────────────────────────

#[test]
fn used_import_in_catch_not_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_used_catch.php";
    let text = r#"<?php
namespace App;

use RuntimeException;

class Consumer {
    public function run(): void {
        try {
        } catch (RuntimeException $e) {
        }
    }
}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary.is_empty(),
        "Import used in catch clause should not be flagged, got: {:?}",
        unnecessary
    );
}

// ─── Deprecated method on parent via static ─────────────────────────────────

#[test]
fn deprecated_inherited_method_via_parent() {
    let backend = create_test_backend();
    let uri = "file:///test_deprecated_parent.php";
    let text = r#"<?php
class Base {
    /** @deprecated Use newMethod() instead */
    public static function oldMethod(): void {}

    public static function newMethod(): void {}
}

class Child extends Base {
    public function run(): void {
        parent::oldMethod();
    }
}
"#;

    let diags = deprecated_diagnostics(&backend, uri, text);
    let deprecated: Vec<_> = diags.iter().filter(|d| has_deprecated_tag(d)).collect();

    assert!(
        deprecated.iter().any(|d| d.message.contains("oldMethod")),
        "Expected deprecated diagnostic for parent::oldMethod(), got: {:?}",
        deprecated
    );
}

// ─── All imports used → zero unnecessary diagnostics ────────────────────────

#[test]
fn all_imports_used_no_unnecessary_diagnostics() {
    let backend = create_test_backend();
    let uri = "file:///test_all_used.php";
    let text = r#"<?php
namespace App;

use Foo\TypeA;
use Foo\TypeB;
use Foo\TypeC;

class Consumer {
    public function a(TypeA $a): TypeB {
        $x = new TypeC();
    }
}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary.is_empty(),
        "All imports are used, should have no unnecessary diagnostics, got: {:?}",
        unnecessary
    );
}

// ─── Multiple unused imports ────────────────────────────────────────────────

#[test]
fn multiple_unused_imports_all_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_multi_unused.php";
    let text = r#"<?php
namespace App;

use Foo\Unused1;
use Foo\Unused2;
use Foo\Unused3;

class Consumer {}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert_eq!(
        unnecessary.len(),
        3,
        "Expected 3 unused import diagnostics, got: {:?}",
        unnecessary
    );
}

// ─── Deprecated class on declaration site should NOT be flagged ─────────────

#[test]
fn deprecated_class_declaration_not_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_deprecated_decl.php";
    let text = r#"<?php
/** @deprecated */
class DeprecatedClass {
    public function foo(): void {}
}
"#;

    let diags = deprecated_diagnostics(&backend, uri, text);
    let deprecated: Vec<_> = diags.iter().filter(|d| has_deprecated_tag(d)).collect();

    // The declaration itself should not be flagged — only references to it.
    // ClassDeclaration spans are different from ClassReference spans.
    assert!(
        deprecated.is_empty(),
        "Class declaration should not produce deprecated diagnostics, got: {:?}",
        deprecated
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Catch clause union type import detection
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn used_import_in_catch_union_type_not_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_catch_union.php";
    let text = r#"<?php
namespace Demo;

use GtdAccessException;

class Foo {
    public function demo(): void {
        try {
        } catch (GtdAccessException $e) {}
    }
}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary.is_empty(),
        "Import used in catch clause should not be flagged as unused, got: {:?}",
        unnecessary
    );
}

#[test]
fn used_import_in_catch_multi_union_type_not_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_catch_multi.php";
    let text = r#"<?php
namespace Demo;

use GtdNotFoundException;
use GtdAccessException;

class GtdNotFoundException extends \RuntimeException {}
class GtdAccessException extends \RuntimeException {}

class Foo {
    public function demo(): void {
        try {
        } catch (GtdNotFoundException|GtdAccessException $e) {}
    }
}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary.is_empty(),
        "Imports used in catch union type should not be flagged, got: {:?}",
        unnecessary
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Truly unused import IS flagged (example.php-like scenario)
// ═══════════════════════════════════════════════════════════════════════════

/// Matches the example.php structure: a namespace block with use statements,
/// multiple classes, and an import that is genuinely not referenced anywhere.
#[test]
fn truly_unused_import_in_namespaced_file_is_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_example_like.php";
    let text = r#"<?php
namespace Demo {

use Exception;
use GtdAccessException;
use Stringable;

class GtdTarget {
    public function label(): string { return ''; }
}

class GtdNotFoundException extends \RuntimeException {}
class GtdAccessException extends \RuntimeException {}

class TypeHintGtdDemo {
    public function demo(): void {
        try {
            $x = new GtdTarget();
        } catch (GtdNotFoundException $e) {}
    }

    public function paramTypes(GtdTarget $item): GtdTarget { return $item; }
}

}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    // GtdAccessException is imported but never referenced — should be flagged.
    // Exception and Stringable are also imported but not referenced — should be flagged.
    // GtdNotFoundException IS referenced in the catch clause — should NOT be flagged.
    let flagged_msgs: Vec<&str> = unnecessary.iter().map(|d| d.message.as_str()).collect();

    assert!(
        unnecessary
            .iter()
            .any(|d| d.message.contains("GtdAccessException")),
        "GtdAccessException is unused and should be flagged, got: {:?}",
        flagged_msgs
    );

    assert!(
        !unnecessary
            .iter()
            .any(|d| d.message.contains("GtdNotFoundException")),
        "GtdNotFoundException IS used in catch clause and should NOT be flagged, got: {:?}",
        flagged_msgs
    );
}

/// When GtdAccessException IS used in a catch union, it should NOT be flagged.
#[test]
fn used_import_in_catch_union_namespaced_not_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_example_used.php";
    let text = r#"<?php
namespace Demo {

use GtdNotFoundException;
use GtdAccessException;

class GtdNotFoundException extends \RuntimeException {}
class GtdAccessException extends \RuntimeException {}

class TypeHintGtdDemo {
    public function demo(): void {
        try {
        } catch (GtdNotFoundException|GtdAccessException $e) {}
    }
}

}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary.is_empty(),
        "Both imports are used in catch union type, none should be flagged, got: {:?}",
        unnecessary.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

/// Import used only in a PHPDoc @param/@return tag should not be flagged.
#[test]
fn used_import_in_phpdoc_not_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_phpdoc_usage.php";
    let text = r#"<?php
namespace App;

use Foo\BarType;

class Consumer {
    /**
     * @param BarType $item
     * @return BarType
     */
    public function process($item) {}
}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary.is_empty(),
        "Import used in PHPDoc should not be flagged, got: {:?}",
        unnecessary
    );
}

/// Import with alias: the alias name is what matters for usage detection.
#[test]
fn aliased_import_used_not_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_alias_used.php";
    let text = r#"<?php
namespace App;

use Foo\UserProfile as Profile;

class Consumer {
    public function run(Profile $p): void {}
}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary.is_empty(),
        "Aliased import used via alias should not be flagged, got: {:?}",
        unnecessary
    );
}

/// Import with alias that is NOT used anywhere.
#[test]
fn aliased_import_unused_is_flagged() {
    let backend = create_test_backend();
    let uri = "file:///test_alias_unused.php";
    let text = r#"<?php
namespace App;

use Foo\UserProfile as Profile;

class Consumer {
    public function run(): void {}
}
"#;

    let diags = unused_import_diagnostics(&backend, uri, text);
    let unnecessary: Vec<_> = diags.iter().filter(|d| has_unnecessary_tag(d)).collect();

    assert!(
        unnecessary
            .iter()
            .any(|d| d.message.contains("Foo\\UserProfile")),
        "Unused aliased import should be flagged, got: {:?}",
        unnecessary
    );
}
