use crate::common::{create_psr4_workspace, create_test_backend};
use tower_lsp::lsp_types::*;

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Open a file, trigger `update_ast`, then collect unknown-member diagnostics.
fn unknown_member_diagnostics(
    backend: &phpantom_lsp::Backend,
    uri: &str,
    text: &str,
) -> Vec<Diagnostic> {
    backend.update_ast(uri, text);
    let mut out = Vec::new();
    backend.collect_unknown_member_diagnostics(uri, text, &mut out);
    out
}

// ═══════════════════════════════════════════════════════════════════════════
// Basic detection — instance methods
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn flags_unknown_instance_method() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    public function bar(): void {}
}

class Consumer {
    public function run(): void {
        $f = new Foo();
        $f->nonexistent();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("nonexistent") && d.message.contains("not found")),
        "Expected unknown method diagnostic, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_existing_instance_method() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    public function bar(): void {}
}

class Consumer {
    public function run(): void {
        $f = new Foo();
        $f->bar();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for existing method, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Basic detection — instance properties
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn flags_unknown_instance_property() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    public string $name = '';
}

class Consumer {
    public function run(): void {
        $f = new Foo();
        $f->missing;
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("missing") && d.message.contains("not found")),
        "Expected unknown property diagnostic, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_existing_instance_property() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    public string $name = '';
}

class Consumer {
    public function run(): void {
        $f = new Foo();
        $f->name;
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for existing property, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Basic detection — static methods
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn flags_unknown_static_method() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    public static function bar(): void {}
}

Foo::nonexistent();
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("nonexistent") && d.message.contains("not found")),
        "Expected unknown static method diagnostic, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_existing_static_method() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    public static function bar(): void {}
}

Foo::bar();
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for existing static method, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Basic detection — constants
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn flags_unknown_class_constant() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    const BAR = 1;
}

$x = Foo::MISSING;
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("MISSING") && d.message.contains("not found")),
        "Expected unknown constant diagnostic, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_existing_class_constant() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    const BAR = 1;
}

$x = Foo::BAR;
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for existing constant, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Static properties
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_existing_static_property() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Config {
    public static string $appName = 'test';
}

$name = Config::$appName;
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for existing static property, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// ::class magic constant
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_class_keyword() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {}

$name = Foo::class;
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for ::class, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Magic method suppression
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn diagnostic_when_class_has_magic_call_but_chain_continues() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Magic {
    public function __call(string $name, array $args): mixed {}
}

class Consumer {
    public function run(): void {
        $m = new Magic();
        $m->anything();
        $m->whatever();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert_eq!(
        diags.len(),
        2,
        "Should flag unknown methods even when __call exists, got: {:?}",
        diags
    );
    assert!(
        diags[0].message.contains("anything"),
        "First diagnostic should mention 'anything', got: {}",
        diags[0].message
    );
    assert!(
        diags[1].message.contains("whatever"),
        "Second diagnostic should mention 'whatever', got: {}",
        diags[1].message
    );
}

#[test]
fn no_diagnostic_when_class_has_magic_get() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class DynProps {
    public function __get(string $name): mixed {}
}

class Consumer {
    public function run(): void {
        $d = new DynProps();
        $d->anything;
        $d->whatever;
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected when __get exists, got: {:?}",
        diags
    );
}

#[test]
fn diagnostic_when_class_has_magic_call_static_but_chain_continues() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class StaticMagic {
    public static function __callStatic(string $name, array $args): mixed {}
}

StaticMagic::anything();
StaticMagic::whatever();
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert_eq!(
        diags.len(),
        2,
        "Should flag unknown static methods even when __callStatic exists, got: {:?}",
        diags
    );
    assert!(
        diags[0].message.contains("anything"),
        "First diagnostic should mention 'anything', got: {}",
        diags[0].message
    );
    assert!(
        diags[1].message.contains("whatever"),
        "Second diagnostic should mention 'whatever', got: {}",
        diags[1].message
    );
}

#[test]
fn magic_call_does_not_suppress_property_diagnostics() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Magic {
    public function __call(string $name, array $args): mixed {}
}

class Consumer {
    public function run(): void {
        $m = new Magic();
        $m->missingProp;
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    // __call only handles method calls, not property access.
    // Without __get, property access should still be flagged.
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("missingProp") && d.message.contains("not found")),
        "Expected unknown property diagnostic even with __call (no __get), got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Inherited magic methods
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn diagnostic_when_parent_has_magic_call_but_chain_continues() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Base {
    public function __call(string $name, array $args): mixed {}
}

class Child extends Base {}

class Consumer {
    public function run(): void {
        $c = new Child();
        $c->anything();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert_eq!(
        diags.len(),
        1,
        "Should flag unknown method even when parent has __call, got: {:?}",
        diags
    );
    assert!(
        diags[0].message.contains("anything"),
        "Diagnostic should mention 'anything', got: {}",
        diags[0].message
    );
}

#[test]
fn no_diagnostic_when_trait_has_magic_get() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
trait DynamicProperties {
    public function __get(string $name): mixed {}
}

class Widget {
    use DynamicProperties;
}

class Consumer {
    public function run(): void {
        $w = new Widget();
        $w->anything;
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected when trait provides __get, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Inheritance — methods, properties, constants
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_inherited_method() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Base {
    public function baseMethod(): void {}
}

class Child extends Base {}

class Consumer {
    public function run(): void {
        $c = new Child();
        $c->baseMethod();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for inherited method, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_inherited_property() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Base {
    public string $baseProp = '';
}

class Child extends Base {}

class Consumer {
    public function run(): void {
        $c = new Child();
        $c->baseProp;
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for inherited property, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_inherited_constant() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Base {
    const BASE_CONST = 42;
}

class Child extends Base {}

$x = Child::BASE_CONST;
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for inherited constant, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Trait members
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_trait_method() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
trait Greetable {
    public function greet(): string { return 'hello'; }
}

class Greeter {
    use Greetable;
}

class Consumer {
    public function run(): void {
        $g = new Greeter();
        $g->greet();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for trait method, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_trait_property() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
trait HasName {
    public string $name = '';
}

class User {
    use HasName;
}

class Consumer {
    public function run(): void {
        $u = new User();
        $u->name;
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for trait property, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Virtual members (@method / @property / @mixin)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_phpdoc_method() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
/**
 * @method string getName()
 * @method void setName(string $name)
 */
class VirtualClass {}

class Consumer {
    public function run(): void {
        $v = new VirtualClass();
        $v->getName();
        $v->setName('test');
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for @method virtual member, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_phpdoc_property() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
/**
 * @property string $name
 * @property-read int $id
 */
class VirtualClass {
    public function __get(string $name): mixed {}
}

class Consumer {
    public function run(): void {
        $v = new VirtualClass();
        $v->name;
        $v->id;
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for @property virtual member, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_mixin_method() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Helper {
    public function doHelp(): void {}
}

/**
 * @mixin Helper
 */
class Service {}

class Consumer {
    public function run(): void {
        $s = new Service();
        $s->doHelp();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for @mixin method, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// $this / self / static / parent contexts
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn flags_unknown_method_on_this() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    public function bar(): void {
        $this->nonexistent();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("nonexistent") && d.message.contains("not found")),
        "Expected unknown method diagnostic for $this->nonexistent(), got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_this_existing_method() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    public function bar(): void {}

    public function baz(): void {
        $this->bar();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for $this->bar(), got: {:?}",
        diags
    );
}

#[test]
fn flags_unknown_method_on_self() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    public function bar(): void {
        self::nonexistent();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("nonexistent") && d.message.contains("not found")),
        "Expected unknown method diagnostic for self::nonexistent(), got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_self_existing_method() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    public static function bar(): void {}

    public function baz(): void {
        self::bar();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for self::bar(), got: {:?}",
        diags
    );
}

#[test]
fn flags_unknown_method_on_static() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    public function bar(): void {
        static::nonexistent();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("nonexistent") && d.message.contains("not found")),
        "Expected unknown method diagnostic for static::nonexistent(), got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_parent_existing_method() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Base {
    public function parentMethod(): void {}
}

class Child extends Base {
    public function childMethod(): void {
        parent::parentMethod();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for parent::parentMethod(), got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Case-insensitive method matching
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn method_matching_is_case_insensitive() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    public function getData(): void {}
}

class Consumer {
    public function run(): void {
        $f = new Foo();
        $f->getdata();
        $f->GETDATA();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "PHP methods are case-insensitive, no diagnostic expected, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Multiple unknown members in one file
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn flags_multiple_unknown_members() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    public function known(): void {}
}

class Consumer {
    public function run(): void {
        $f = new Foo();
        $f->unknown1();
        $f->known();
        $f->unknown2();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert_eq!(
        diags.len(),
        2,
        "Expected exactly 2 diagnostics, got: {:?}",
        diags
    );
    assert!(diags.iter().any(|d| d.message.contains("unknown1")));
    assert!(diags.iter().any(|d| d.message.contains("unknown2")));
}

// ═══════════════════════════════════════════════════════════════════════════
// Unresolvable subject — no false positives
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_when_subject_unresolvable() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
function getUnknown(): mixed { return null; }

$x = getUnknown();
$x->whatever();
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected when subject type is unresolvable, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_when_class_not_found() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
UnknownClass::method();
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    // The class itself is unknown — that's a different diagnostic
    // (unknown_classes). We should not also flag the member.
    assert!(
        diags.is_empty(),
        "No member diagnostic expected when the class itself is unknown, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Enum cases
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_enum_case() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
enum Color {
    case Red;
    case Green;
    case Blue;
}

$c = Color::Red;
$d = Color::Green;
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for enum case access, got: {:?}",
        diags
    );
}

#[test]
fn flags_unknown_enum_case() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
enum Color {
    case Red;
    case Green;
    case Blue;
}

$c = Color::Purple;
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("Purple") && d.message.contains("not found")),
        "Expected unknown member diagnostic for Color::Purple, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_backed_enum_case() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
enum Status: string {
    case Active = 'active';
    case Inactive = 'inactive';
}

$s = Status::Active;
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for backed enum case, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Parameter type hint resolution
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn flags_unknown_method_via_parameter_type_hint() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Service {
    public function doWork(): void {}
}

class Handler {
    public function handle(Service $svc): void {
        $svc->nonexistent();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("nonexistent") && d.message.contains("not found")),
        "Expected unknown method diagnostic via parameter type, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_method_via_parameter_type_hint() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Service {
    public function doWork(): void {}
}

class Handler {
    public function handle(Service $svc): void {
        $svc->doWork();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for existing method via parameter, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_method_via_param_docblock_override() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Node {}

class FuncCall extends Node {
    public function isFirstClassCallable(): bool {}
}

class Handler {
    /**
     * @param FuncCall $node
     */
    public function handle(Node $node): void {
        $node->isFirstClassCallable();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for existing method via @param override, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Interface method access
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_interface_method() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
interface Renderable {
    public function render(): string;
}

class View implements Renderable {
    public function render(): string { return ''; }
}

class Consumer {
    public function run(Renderable $r): void {
        $r->render();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for interface method, got: {:?}",
        diags
    );
}

#[test]
fn flags_unknown_method_on_interface() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
interface Renderable {
    public function render(): string;
}

class Consumer {
    public function run(Renderable $r): void {
        $r->nonexistent();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("nonexistent") && d.message.contains("not found")),
        "Expected unknown method diagnostic on interface, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Diagnostic metadata
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn diagnostic_has_warning_severity() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {}

class Consumer {
    public function run(): void {
        $f = new Foo();
        $f->missing();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(!diags.is_empty(), "Expected at least one diagnostic");
    assert_eq!(diags[0].severity, Some(DiagnosticSeverity::WARNING));
}

#[test]
fn diagnostic_has_code_and_source() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {}

class Consumer {
    public function run(): void {
        $f = new Foo();
        $f->missing();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(!diags.is_empty(), "Expected at least one diagnostic");
    assert_eq!(
        diags[0].code,
        Some(NumberOrString::String("unknown_member".to_string()))
    );
    assert_eq!(diags[0].source, Some("phpantom".to_string()));
}

#[test]
fn diagnostic_message_includes_class_name() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class MyService {}

class Consumer {
    public function run(): void {
        $s = new MyService();
        $s->missing();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(!diags.is_empty(), "Expected at least one diagnostic");
    assert!(
        diags[0].message.contains("MyService"),
        "Diagnostic should mention the class name, got: {}",
        diags[0].message
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Constructor calls should not flag
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_constructor_call() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    public function __construct() {}
}

$f = new Foo();
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for constructor call, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Method return type chain resolution
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_method_chain_existing_members() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Builder {
    public function where(): Builder { return $this; }
    public function get(): array { return []; }
}

class Service {
    public function query(): Builder { return new Builder(); }
}

class Consumer {
    public function run(): void {
        $s = new Service();
        $s->query()->where();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for valid method chain, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Cross-file resolution (PSR-4)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn flags_unknown_member_cross_file() {
    let (backend, _dir) = create_psr4_workspace(
        r#"{ "autoload": { "psr-4": { "App\\": "src/" } } }"#,
        &[(
            "src/Service.php",
            r#"<?php
namespace App;

class Service {
    public function doWork(): void {}
}
"#,
        )],
    );

    let uri = "file:///consumer.php";
    let text = r#"<?php
use App\Service;

class Consumer {
    public function run(Service $svc): void {
        $svc->nonexistent();
    }
}
"#;
    backend.update_ast(uri, text);
    let mut diags = Vec::new();
    backend.collect_unknown_member_diagnostics(uri, text, &mut diags);

    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("nonexistent") && d.message.contains("not found")),
        "Expected unknown method diagnostic across files, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_existing_member_cross_file() {
    let (backend, _dir) = create_psr4_workspace(
        r#"{ "autoload": { "psr-4": { "App\\": "src/" } } }"#,
        &[(
            "src/Service.php",
            r#"<?php
namespace App;

class Service {
    public function doWork(): void {}
}
"#,
        )],
    );

    let uri = "file:///consumer.php";
    let text = r#"<?php
use App\Service;

class Consumer {
    public function run(Service $svc): void {
        $svc->doWork();
    }
}
"#;
    backend.update_ast(uri, text);
    let mut diags = Vec::new();
    backend.collect_unknown_member_diagnostics(uri, text, &mut diags);

    assert!(
        diags.is_empty(),
        "No diagnostics expected for existing member across files, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Mixed known and unknown in same access chain
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn only_flags_the_unknown_member_not_the_known() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    public function bar(): void {}
    public string $name = '';
}

class Consumer {
    public function run(): void {
        $f = new Foo();
        $f->bar();
        $f->name;
        $f->missing;
        $f->alsoMissing();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert_eq!(
        diags.len(),
        2,
        "Expected exactly 2 diagnostics (missing, alsoMissing), got: {:?}",
        diags
    );
    assert!(
        !diags.iter().any(|d| d.message.contains("'bar'")),
        "bar() should not be flagged"
    );
    assert!(
        !diags.iter().any(|d| d.message.contains("'name'")),
        "name should not be flagged"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Abstract class members
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_abstract_method() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
abstract class Shape {
    abstract public function area(): float;
}

class Consumer {
    public function run(Shape $s): void {
        $s->area();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for abstract method, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Promoted constructor properties
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_promoted_property() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class User {
    public function __construct(
        public readonly string $name,
        public readonly string $email,
    ) {}
}

class Consumer {
    public function run(): void {
        $u = new User('John', 'john@example.com');
        $u->name;
        $u->email;
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for promoted properties, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Visibility should not affect detection
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_private_method_on_this() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    private function secret(): void {}

    public function bar(): void {
        $this->secret();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    // We don't check visibility — the member exists, so no diagnostic.
    // Visibility violations are a different diagnostic (not implemented yet).
    assert!(
        diags.is_empty(),
        "No diagnostics expected for private method via $this, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_protected_method_on_this() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    protected function helper(): void {}

    public function bar(): void {
        $this->helper();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for protected method via $this, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Empty class produces diagnostic
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn flags_method_on_empty_class() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Empty_ {}

class Consumer {
    public function run(): void {
        $e = new Empty_();
        $e->anything();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("anything") && d.message.contains("not found")),
        "Expected unknown method diagnostic on empty class, got: {:?}",
        diags
    );
}

#[test]
fn flags_property_on_empty_class() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Empty_ {}

class Consumer {
    public function run(): void {
        $e = new Empty_();
        $e->anything;
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("anything") && d.message.contains("not found")),
        "Expected unknown property diagnostic on empty class, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Enum constant access (not a case)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_enum_constant() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
enum Color {
    case Red;
    const DEFAULT = self::Red;
}

$x = Color::DEFAULT;
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for enum constant, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Interface virtual members (@method on interface)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_interface_phpdoc_method() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
/**
 * @method string format()
 */
interface Formattable {}

class Widget implements Formattable {}

class Consumer {
    public function run(): void {
        $w = new Widget();
        $w->format();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for interface @method, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Self constant access
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_self_constant() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    const MAX = 100;

    public function getMax(): int {
        return self::MAX;
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for self::MAX, got: {:?}",
        diags
    );
}

#[test]
fn flags_unknown_self_constant() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Foo {
    const MAX = 100;

    public function getMin(): int {
        return self::MIN;
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("MIN") && d.message.contains("not found")),
        "Expected unknown constant diagnostic for self::MIN, got: {:?}",
        diags
    );
}

// ── stdClass suppression ────────────────────────────────────────────────

/// stdClass is a universal object container — any property access on it
/// should be silently accepted.
#[test]
fn no_diagnostic_for_property_on_stdclass() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
$obj = new \stdClass();
$obj->anything;
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for property access on stdClass, got: {:?}",
        diags
    );
}

/// Method calls on stdClass should also be suppressed.
#[test]
fn no_diagnostic_for_method_on_stdclass() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
$obj = new \stdClass();
$obj->whatever();
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for method call on stdClass, got: {:?}",
        diags
    );
}

/// When stdClass appears as a branch in a union type, suppress diagnostics
/// for the entire union since the property could be on the stdClass branch.
#[test]
fn no_diagnostic_for_stdclass_in_union() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Strict {
    public function known(): void {}
}

/** @var Strict|\stdClass $obj */
$obj = new Strict();
$obj->unknown_prop;
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected when any union branch is stdClass, got: {:?}",
        diags
    );
}

/// stdClass passed as a parameter type hint should suppress diagnostics.
#[test]
fn no_diagnostic_for_stdclass_parameter() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
function process(\stdClass $obj): void {
    $obj->foo;
    $obj->bar;
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for property access on stdClass parameter, got: {:?}",
        diags
    );
}

/// A method returning stdClass should suppress diagnostics on the result.
#[test]
fn no_diagnostic_for_stdclass_return_type() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Factory {
    public function create(): \stdClass {
        return new \stdClass();
    }
}
$f = new Factory();
$f->create()->name;
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for property access on stdClass return type, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Method return → array access: $c->items()[0]->getLabel()
// ═══════════════════════════════════════════════════════════════════════════

/// When a method returns `Item[]` and the caller indexes inline
/// (`$c->items()[0]->getLabel()`), the element type should resolve
/// and no false "cannot verify" warning should appear.
#[test]
fn no_diagnostic_for_method_return_array_access_bracket_type() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Item {
    public function getLabel(): string { return ''; }
}
class Collection {
    /** @return Item[] */
    public function items(): array { return []; }
}
class Consumer {
    public function run(): void {
        $c = new Collection();
        $c->items()[0]->getLabel();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        !diags.iter().any(|d| d.message.contains("getLabel")),
        "No diagnostic expected for getLabel on Item resolved via method-return array access, got: {:?}",
        diags
    );
}

/// Same pattern with `array<int, Item>` generic return type.
#[test]
fn no_diagnostic_for_method_return_array_access_generic_type() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Item {
    public function getLabel(): string { return ''; }
}
class Collection {
    /** @return array<int, Item> */
    public function items(): array { return []; }
}
class Consumer {
    public function run(): void {
        $c = new Collection();
        $c->items()[0]->getLabel();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        !diags.iter().any(|d| d.message.contains("getLabel")),
        "No diagnostic expected for getLabel on Item resolved via generic method-return array access, got: {:?}",
        diags
    );
}

/// Static method returning an array: `Collection::all()[0]->getLabel()`.
#[test]
fn no_diagnostic_for_function_return_type_resolved_cross_file() {
    // Regression test: standalone functions store return types as short
    // names from the declaring file.  After FQN resolution in update_ast,
    // consumers in other files should resolve the type correctly.
    let (backend, _dir) = create_psr4_workspace(
        r#"{ "autoload": { "psr-4": { "App\\": "src/" } } }"#,
        &[(
            "src/Clock.php",
            r#"<?php
namespace App;

interface Clock {
    public function subMinutes(int $value = 1): Clock;
}
"#,
        )],
    );

    // A helper file that imports Clock via `use` and returns the short name.
    let helpers_uri = "file:///helpers.php";
    let helpers = r#"<?php
use App\Clock;

function now(): Clock {
    // stub
}
"#;
    backend.update_ast(helpers_uri, helpers);

    // Consumer file does NOT import App\Clock — it relies on the
    // function's return type being resolved to FQN at parse time.
    let uri = "file:///test.php";
    let text = r#"<?php
class Consumer {
    public function run(): void {
        now()->subMinutes(5);
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        !diags.iter().any(|d| d.message.contains("subMinutes")),
        "No diagnostic expected for subMinutes on function return type resolved via FQN, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_static_method_return_array_access() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Item {
    public function getLabel(): string { return ''; }
}
class Collection {
    /** @return Item[] */
    public static function all(): array { return []; }
}

function test(): void {
    Collection::all()[0]->getLabel();
}
"#;
    backend.update_ast(uri, text);
    let mut diags = Vec::new();
    backend.collect_unknown_member_diagnostics(uri, text, &mut diags);
    assert!(
        !diags.iter().any(|d| d.message.contains("getLabel")),
        "No diagnostic expected for getLabel on Item resolved via static method-return array access, got: {:?}",
        diags
    );
}

/// `$app['config']->set(...)` where `Application implements ArrayAccess`
/// without concrete generic annotations should NOT resolve the bracket
/// access to `Application` itself.  With `unresolved-member-access`
/// enabled, it should emit a diagnostic saying the type could not be
/// resolved.
#[test]
fn array_access_on_array_access_class_emits_unresolved_diagnostic() {
    let backend = create_test_backend();
    {
        let mut cfg = backend.config();
        cfg.diagnostics.unresolved_member_access = Some(true);
        backend.set_config(cfg);
    }
    let uri = "file:///test.php";
    let text = r#"<?php
class Application implements \ArrayAccess {
    public function offsetExists(mixed $offset): bool { return true; }
    public function offsetGet(mixed $offset): mixed { return null; }
    public function offsetSet(mixed $offset, mixed $value): void {}
    public function offsetUnset(mixed $offset): void {}

    public function useStoragePath(string $path): void {}
}

function test(Application $app): void {
    $app->useStoragePath('/tmp');
    $app['config']->set('logging.default', 'stderr');
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    // $app->useStoragePath() should NOT be flagged (valid method).
    assert!(
        !diags.iter().any(|d| d.message.contains("useStoragePath")),
        "useStoragePath is a valid method on Application, got: {diags:?}",
    );
    // $app['config']->set() should NOT say 'set' is missing on Application.
    assert!(
        !diags.iter().any(|d| d.message.contains("Application")),
        "should not report 'set' as missing on Application, got: {diags:?}",
    );
    // $app['config']->set() SHOULD flag that the subject type is unresolved.
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("set") && d.message.contains("could not be resolved")),
        "expected unresolved-member-access diagnostic for $app['config']->set(), got: {diags:?}",
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Assert narrowing boundary prevents stale diagnostic cache reuse
// ═══════════════════════════════════════════════════════════════════════════

/// When a variable is used in a member access *before* an
/// `assert($var instanceof X)` and then used again *after* the assert,
/// the diagnostic cache must not reuse the pre-assert resolution.
/// Without the assert-offset discriminator in the cache key, the second
/// access would reuse the cached pre-assert type and produce a false
/// positive "property not found" diagnostic.
///
/// This reproduces the real-world Mockery pattern: `mock()` returns
/// `MockInterface`, the test calls `->shouldReceive()` (valid on
/// `MockInterface`), then `assert($x instanceof ConcreteClass)` narrows
/// the type so that `->id` (a property on the concrete class) is valid.
#[test]
fn no_false_positive_after_assert_instanceof() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
interface MockInterface {
    public function shouldReceive(string $name): self;
}
class MolliePayment {
    public string $id = '';
    public function canBeRefunded(): bool { return true; }
}
class TestCase {
    protected function mock(string $class): MockInterface {}
}
class Test extends TestCase {
    public function test(): void {
        $x = $this->mock(MolliePayment::class);
        $x->shouldReceive('canBeRefunded');
        assert($x instanceof MolliePayment);
        echo $x->id;
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        !diags.iter().any(|d| d.message.contains("id")),
        "No diagnostic expected for 'id' after assert($x instanceof MolliePayment), got: {:?}",
        diags
    );
}

/// Verify that the pre-assert access is still correctly diagnosed when
/// the member does NOT exist on the pre-assert type.
#[test]
fn still_flags_unknown_member_before_assert_instanceof() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
interface MockInterface {
    public function shouldReceive(string $name): self;
}
class MolliePayment {
    public string $id = '';
    public function canBeRefunded(): bool { return true; }
}
class TestCase {
    protected function mock(string $class): MockInterface {}
}
class Test extends TestCase {
    public function test(): void {
        $x = $this->mock(MolliePayment::class);
        echo $x->id;
        assert($x instanceof MolliePayment);
        echo $x->id;
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    // The first $x->id (before the assert) should be flagged because
    // $x is MockInterface and MockInterface has no 'id' property.
    let id_diags: Vec<_> = diags.iter().filter(|d| d.message.contains("id")).collect();
    assert_eq!(
        id_diags.len(),
        1,
        "Expected exactly one diagnostic for 'id' (the pre-assert access), got: {:?}",
        id_diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Static return type resolution to concrete subclass
// ═══════════════════════════════════════════════════════════════════════════

/// When a parent class declares `public static function first(): ?static`,
/// calling `ChildClass::first()` should resolve `static` to `ChildClass`,
/// not the parent. No false-positive diagnostics should be emitted for
/// members that exist on the child class.
#[test]
fn no_diagnostic_for_static_return_type_on_subclass_static_call() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Model {
    /** @return ?static */
    public static function first(): ?static { return null; }
    public function save(): bool { return true; }
}
class AdminUser extends Model {
    public function assignRole(string $role): void {}
}
class Seeder {
    public function run(): void {
        $admin = AdminUser::first();
        $admin->assignRole('admin');
        $admin->save();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected when static return type resolves to subclass, got: {:?}",
        diags
    );
}

/// Same scenario but with a bare `static` return (non-nullable).
#[test]
fn no_diagnostic_for_bare_static_return_type_on_subclass() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Builder {
    /** @return static */
    public static function create(): static { return new static(); }
    public function build(): void {}
}
class AppBuilder extends Builder {
    public function setDebug(): void {}
}
class Factory {
    public function make(): void {
        $b = AppBuilder::create();
        $b->setDebug();
        $b->build();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for bare static return on subclass, got: {:?}",
        diags
    );
}

/// Chained static method calls: `Product::query()->where('x')->get()`
/// where `query()` and `where()` both return `static`.
#[test]
fn no_diagnostic_for_static_return_chained_static_call() {
    let backend = create_test_backend();
    let uri = "file:///test.php";
    let text = r#"<?php
class Model {
    /** @return static */
    public static function query(): static { return new static(); }
    /** @return static */
    public function where(string $col): static { return $this; }
    public function get(): array { return []; }
}
class Product extends Model {
    public function applyDiscount(): void {}
}
class Controller {
    public function index(): void {
        $q = Product::query();
        $q->where('active');
        $q->applyDiscount();
        $q->get();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    assert!(
        diags.is_empty(),
        "No diagnostics expected for chained static return calls, got: {:?}",
        diags
    );
}

/// Cross-file variant: parent with `?static` return lives in a separate
/// PSR-4 file. Accessing subclass-specific members after a static method
/// call should not produce false-positive diagnostics.
#[test]
fn no_diagnostic_for_static_return_type_cross_file() {
    let (backend, _dir) = create_psr4_workspace(
        r#"{ "autoload": { "psr-4": { "App\\": "src/" } } }"#,
        &[
            (
                "src/Model.php",
                r#"<?php
namespace App;

class Model {
    /** @return ?static */
    public static function first(): ?static { return null; }
    public function save(): bool { return true; }
}
"#,
            ),
            (
                "src/AdminUser.php",
                r#"<?php
namespace App;

class AdminUser extends Model {
    public function assignRole(string $role): void {}
}
"#,
            ),
        ],
    );

    let uri = "file:///consumer.php";
    let text = r#"<?php
use App\AdminUser;

class Seeder {
    public function run(): void {
        $admin = AdminUser::first();
        $admin->assignRole('admin');
        $admin->save();
    }
}
"#;
    backend.update_ast(uri, text);
    let mut diags = Vec::new();
    backend.collect_unknown_member_diagnostics(uri, text, &mut diags);

    assert!(
        diags.is_empty(),
        "No diagnostics expected when static return type resolves to subclass cross-file, got: {:?}",
        diags
    );
}

// ─── Eloquent relationship property diagnostics (B4) ────────────────────────

#[test]
fn no_diagnostic_for_relationship_property_on_model() {
    // When a model has a relationship method (e.g. translations() returning
    // HasMany<Translation>), the LaravelModelProvider synthesizes a virtual
    // property `$translations` typed as Collection<Translation>.  Accessing
    // this property should not produce a diagnostic.
    let (backend, _dir) = create_psr4_workspace(
        r#"{ "autoload": { "psr-4": { "App\\": "src/", "Illuminate\\": "illuminate/" } } }"#,
        &[
            (
                "illuminate/Database/Eloquent/Model.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

class Model {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Relations/HasMany.php",
                r#"<?php
namespace Illuminate\Database\Eloquent\Relations;

/**
 * @template TRelatedModel
 * @template TDeclaringModel
 */
class HasMany {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Collection.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

/**
 * @template TModel
 */
class Collection {
    /** @return TModel|null */
    public function first(): mixed { return null; }
}
"#,
            ),
            (
                "src/Translation.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;

class Translation extends Model {
    public string $locale;
}
"#,
            ),
            (
                "src/Category.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;

class Category extends Model {
    /** @return HasMany<Translation, $this> */
    public function translations(): HasMany { return $this->hasMany(Translation::class); }
}
"#,
            ),
        ],
    );

    let uri = "file:///consumer.php";
    let text = r#"<?php
use App\Category;

class Service {
    public function test(Category $cat): void {
        $items = $cat->translations;
    }
}
"#;
    backend.update_ast(uri, text);
    let mut diags = Vec::new();
    backend.collect_unknown_member_diagnostics(uri, text, &mut diags);

    assert!(
        !diags.iter().any(|d| d.message.contains("translations")),
        "Relationship property 'translations' should be resolved via LaravelModelProvider, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_has_one_relationship_property_on_model() {
    // HasOne relationship produces a virtual property typed as the related model.
    let (backend, _dir) = create_psr4_workspace(
        r#"{ "autoload": { "psr-4": { "App\\": "src/", "Illuminate\\": "illuminate/" } } }"#,
        &[
            (
                "illuminate/Database/Eloquent/Model.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

class Model {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Relations/HasOne.php",
                r#"<?php
namespace Illuminate\Database\Eloquent\Relations;

/**
 * @template TRelatedModel
 * @template TDeclaringModel
 */
class HasOne {}
"#,
            ),
            (
                "src/ImageFile.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;

class ImageFile extends Model {
    public string $path;
}
"#,
            ),
            (
                "src/Notification.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasOne;

class Notification extends Model {
    /** @return HasOne<ImageFile, $this> */
    public function imageFile(): HasOne { return $this->hasOne(ImageFile::class); }
}
"#,
            ),
        ],
    );

    let uri = "file:///consumer.php";
    let text = r#"<?php
use App\Notification;

class Handler {
    public function process(Notification $notif): void {
        $file = $notif->imageFile;
    }
}
"#;
    backend.update_ast(uri, text);
    let mut diags = Vec::new();
    backend.collect_unknown_member_diagnostics(uri, text, &mut diags);

    assert!(
        !diags.iter().any(|d| d.message.contains("imageFile")),
        "HasOne relationship property 'imageFile' should be resolved via LaravelModelProvider, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_this_relationship_property_inside_model() {
    // Accessing $this->translations inside the model itself (e.g. in a
    // method body) should resolve the virtual relationship property.
    let (backend, _dir) = create_psr4_workspace(
        r#"{ "autoload": { "psr-4": { "App\\": "src/", "Illuminate\\": "illuminate/" } } }"#,
        &[
            (
                "illuminate/Database/Eloquent/Model.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

class Model {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Relations/HasMany.php",
                r#"<?php
namespace Illuminate\Database\Eloquent\Relations;

/**
 * @template TRelatedModel
 * @template TDeclaringModel
 */
class HasMany {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Collection.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

/**
 * @template TModel
 */
class Collection {
    /** @return TModel|null */
    public function first(): mixed { return null; }
}
"#,
            ),
            (
                "src/Translation.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;

class Translation extends Model {
    public string $locale;
}
"#,
            ),
        ],
    );

    let uri = "file:///src/Category.php";
    let text = r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;

class Category extends Model {
    /** @return HasMany<Translation, $this> */
    public function translations(): HasMany { return $this->hasMany(Translation::class); }

    public function defaultTranslation(): ?Translation {
        return $this->translations->first();
    }
}
"#;
    backend.update_ast(uri, text);
    let mut diags = Vec::new();
    backend.collect_unknown_member_diagnostics(uri, text, &mut diags);

    assert!(
        !diags.iter().any(|d| d.message.contains("translations")),
        "Relationship property '$this->translations' should be resolved inside model, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_belongs_to_associate_method() {
    // Calling a relationship method WITH () returns the relationship object
    // (e.g. BelongsTo).  Methods like associate() should be found on it.
    let (backend, _dir) = create_psr4_workspace(
        r#"{ "autoload": { "psr-4": { "App\\": "src/", "Illuminate\\": "illuminate/" } } }"#,
        &[
            (
                "illuminate/Database/Eloquent/Model.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

class Model {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Relations/BelongsTo.php",
                r#"<?php
namespace Illuminate\Database\Eloquent\Relations;

/**
 * @template TRelatedModel
 * @template TDeclaringModel
 */
class BelongsTo {
    /** @return TDeclaringModel */
    public function associate(mixed $model): static { return $this; }
    public function dissociate(): static { return $this; }
}
"#,
            ),
            (
                "src/ParentModel.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;

class ParentModel extends Model {
    public string $name;
}
"#,
            ),
            (
                "src/ChildModel.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\BelongsTo;

class ChildModel extends Model {
    /** @return BelongsTo<ParentModel, $this> */
    public function parent(): BelongsTo { return $this->belongsTo(ParentModel::class); }
}
"#,
            ),
        ],
    );

    let uri = "file:///consumer.php";
    let text = r#"<?php
use App\ChildModel;
use App\ParentModel;

class Service {
    public function link(ChildModel $child, ParentModel $parent): void {
        $child->parent()->associate($parent);
    }
}
"#;
    backend.update_ast(uri, text);
    let mut diags = Vec::new();
    backend.collect_unknown_member_diagnostics(uri, text, &mut diags);

    assert!(
        !diags.iter().any(|d| d.message.contains("associate")),
        "BelongsTo::associate() should be resolved on relationship method return type, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_belongs_to_with_covariant_this() {
    // When the return type uses `covariant $this` syntax
    // (e.g. BelongsTo<Category, covariant $this>), the type parser
    // should still resolve the BelongsTo class and find its methods.
    let (backend, _dir) = create_psr4_workspace(
        r#"{ "autoload": { "psr-4": { "App\\": "src/", "Illuminate\\": "illuminate/" } } }"#,
        &[
            (
                "illuminate/Database/Eloquent/Model.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

class Model {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Relations/BelongsTo.php",
                r#"<?php
namespace Illuminate\Database\Eloquent\Relations;

/**
 * @template TRelatedModel
 * @template TDeclaringModel
 */
class BelongsTo {
    /** @return TDeclaringModel */
    public function associate(mixed $model): static { return $this; }
    public function dissociate(): static { return $this; }
}
"#,
            ),
            (
                "src/Category.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;

class Category extends Model {}
"#,
            ),
            (
                "src/Translation.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\BelongsTo;

class Translation extends Model {
    /** @return BelongsTo<Category, covariant $this> */
    public function category(): BelongsTo { return $this->belongsTo(Category::class); }
}
"#,
            ),
        ],
    );

    let uri = "file:///consumer.php";
    let text = r#"<?php
use App\Translation;
use App\Category;

class Service {
    public function link(Translation $trans, Category $cat): void {
        $trans->category()->associate($cat);
    }
}
"#;
    backend.update_ast(uri, text);
    let mut diags = Vec::new();
    backend.collect_unknown_member_diagnostics(uri, text, &mut diags);

    assert!(
        !diags.iter().any(|d| d.message.contains("associate")),
        "BelongsTo::associate() should be resolved even with 'covariant $this' syntax, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_relationship_property_inferred_from_body() {
    // When a relationship method has no @return annotation but the body
    // contains `$this->hasMany(Related::class)`, the parser infers the
    // return type and the LaravelModelProvider should synthesize a virtual
    // property from it.
    let (backend, _dir) = create_psr4_workspace(
        r#"{ "autoload": { "psr-4": { "App\\": "src/", "Illuminate\\": "illuminate/" } } }"#,
        &[
            (
                "illuminate/Database/Eloquent/Model.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

class Model {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Relations/HasMany.php",
                r#"<?php
namespace Illuminate\Database\Eloquent\Relations;

/**
 * @template TRelatedModel
 * @template TDeclaringModel
 */
class HasMany {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Collection.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

/**
 * @template TModel
 */
class Collection {}
"#,
            ),
            (
                "src/Comment.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;

class Comment extends Model {
    public string $body;
}
"#,
            ),
            (
                "src/Post.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;

class Post extends Model {
    public function comments() { return $this->hasMany(Comment::class); }
}
"#,
            ),
        ],
    );

    let uri = "file:///consumer.php";
    let text = r#"<?php
use App\Post;

class Handler {
    public function test(Post $post): void {
        $items = $post->comments;
    }
}
"#;
    backend.update_ast(uri, text);
    let mut diags = Vec::new();
    backend.collect_unknown_member_diagnostics(uri, text, &mut diags);

    assert!(
        !diags.iter().any(|d| d.message.contains("comments")),
        "Body-inferred relationship property 'comments' should be resolved, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_relationship_property_with_mixed_native_return() {
    // In real Laravel projects, relationship methods often declare `mixed`
    // as the native return type with the specific relationship type only
    // in the @return docblock.  The LaravelModelProvider must still
    // synthesize the virtual property from the docblock return type.
    let (backend, _dir) = create_psr4_workspace(
        r#"{ "autoload": { "psr-4": { "App\\": "src/", "Illuminate\\": "illuminate/" } } }"#,
        &[
            (
                "illuminate/Database/Eloquent/Model.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

class Model {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Relations/HasMany.php",
                r#"<?php
namespace Illuminate\Database\Eloquent\Relations;

/**
 * @template TRelatedModel
 * @template TDeclaringModel
 */
class HasMany {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Relations/HasOne.php",
                r#"<?php
namespace Illuminate\Database\Eloquent\Relations;

/**
 * @template TRelatedModel
 * @template TDeclaringModel
 */
class HasOne {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Relations/BelongsTo.php",
                r#"<?php
namespace Illuminate\Database\Eloquent\Relations;

/**
 * @template TRelatedModel
 * @template TDeclaringModel
 */
class BelongsTo {
    /** @return TDeclaringModel */
    public function associate(mixed $model): static { return $this; }
    public function dissociate(): static { return $this; }
}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Collection.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

/**
 * @template TModel
 */
class Collection {
    /** @return TModel|null */
    public function first(): mixed { return null; }
}
"#,
            ),
            (
                "src/Translation.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;

class Translation extends Model {
    public string $locale;
}
"#,
            ),
            (
                "src/ImageFile.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;

class ImageFile extends Model {
    public string $path;
}
"#,
            ),
            (
                "src/Category.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;
use Illuminate\Database\Eloquent\Relations\BelongsTo;

class Category extends Model {
    public string $name;
}
"#,
            ),
            (
                "src/NotificationCategory.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasMany;

class NotificationCategory extends Model {
    /**
     * @return HasMany<Translation, $this>
     */
    public function translations(): mixed { return $this->hasMany(Translation::class); }

    public function defaultTranslation(): mixed {
        return $this->translations->first();
    }
}
"#,
            ),
            (
                "src/NotificationObject.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\HasOne;

class NotificationObject extends Model {
    /**
     * @return HasOne<ImageFile, $this>
     */
    public function imageFile(): mixed { return $this->hasOne(ImageFile::class); }

    public function getImagePath(): mixed {
        return $this->imageFile->path;
    }
}
"#,
            ),
            (
                "src/TranslationModel.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\BelongsTo;

class TranslationModel extends Model {
    /**
     * @return BelongsTo<Category, covariant $this>
     */
    public function category(): mixed { return $this->belongsTo(Category::class); }
}
"#,
            ),
        ],
    );

    // Test 1: $this->translations inside model (HasMany virtual property)
    let uri1 = "file:///src/NotificationCategory.php";
    let text1 = std::fs::read_to_string(_dir.path().join("src/NotificationCategory.php")).unwrap();
    backend.update_ast(uri1, &text1);
    let mut diags1 = Vec::new();
    backend.collect_unknown_member_diagnostics(uri1, &text1, &mut diags1);
    assert!(
        !diags1.iter().any(|d| d.message.contains("translations")),
        "HasMany relationship property '$this->translations' with mixed native return should resolve, got: {:?}",
        diags1
    );

    // Test 2: $this->imageFile inside model (HasOne virtual property)
    let uri2 = "file:///src/NotificationObject.php";
    let text2 = std::fs::read_to_string(_dir.path().join("src/NotificationObject.php")).unwrap();
    backend.update_ast(uri2, &text2);
    let mut diags2 = Vec::new();
    backend.collect_unknown_member_diagnostics(uri2, &text2, &mut diags2);
    assert!(
        !diags2.iter().any(|d| d.message.contains("imageFile")),
        "HasOne relationship property '$this->imageFile' with mixed native return should resolve, got: {:?}",
        diags2
    );

    // Test 3: $translation->category()->associate() (BelongsTo with covariant $this)
    let uri3 = "file:///consumer.php";
    let text3 = r#"<?php
use App\TranslationModel;
use App\Category;

class NotificationCategoryService {
    public function link(TranslationModel $translation, Category $cat): void {
        $translation->category()->associate($cat);
    }
}
"#;
    backend.update_ast(uri3, text3);
    let mut diags3 = Vec::new();
    backend.collect_unknown_member_diagnostics(uri3, text3, &mut diags3);
    assert!(
        !diags3.iter().any(|d| d.message.contains("associate")),
        "BelongsTo::associate() should be found when method returns mixed with @return BelongsTo<..., covariant $this>, got: {:?}",
        diags3
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// @mixin with template parameter resolved via property generic type
// ═══════════════════════════════════════════════════════════════════════════

/// When a class declares `@template TWraps` and `@mixin TWraps`, and a
/// property is typed as `Wrapper<ConcreteApi>`, calling methods from
/// `ConcreteApi` on the property should NOT produce unknown_member
/// diagnostics.  This is the Klaviyo SDK pattern.
#[test]
fn no_diagnostic_for_mixin_template_param_via_property_generic() {
    let backend = create_test_backend();

    let wrapper_uri = "file:///Subclient.php";
    let wrapper_text = r#"<?php
/**
 * @template TWraps of object
 * @mixin TWraps
 */
class Subclient {
    public function getApiInstance(): object {}
}
"#;

    let api_uri = "file:///EventsApi.php";
    let api_text = r#"<?php
class EventsApi {
    public function createEvent(array $body): array {}
    public function getEvents(string $filter): array {}
}
"#;

    let consumer_uri = "file:///KlaviyoClient.php";
    let consumer_text = r#"<?php
class KlaviyoClient {
    /** @var Subclient<EventsApi> */
    public $Events;

    function test() {
        $this->Events->createEvent([]);
        $this->Events->getEvents('filter');
        $this->Events->getApiInstance();
    }
}
"#;

    backend.update_ast(wrapper_uri, wrapper_text);
    backend.update_ast(api_uri, api_text);
    backend.update_ast(consumer_uri, consumer_text);

    let diags = unknown_member_diagnostics(&backend, consumer_uri, consumer_text);
    assert!(
        !diags.iter().any(|d| d.message.contains("createEvent")),
        "createEvent from mixin TWraps→EventsApi should not be flagged, got: {:?}",
        diags
    );
    assert!(
        !diags.iter().any(|d| d.message.contains("getEvents")),
        "getEvents from mixin TWraps→EventsApi should not be flagged, got: {:?}",
        diags
    );
    assert!(
        !diags.iter().any(|d| d.message.contains("getApiInstance")),
        "Own method getApiInstance should not be flagged, got: {:?}",
        diags
    );
}

/// Calling a method that does NOT exist on the concrete mixin target
/// should still be flagged as unknown_member.
#[test]
fn diagnostic_for_nonexistent_method_on_mixin_template_param() {
    let backend = create_test_backend();

    let wrapper_uri = "file:///Wrapper.php";
    let wrapper_text = r#"<?php
/**
 * @template T of object
 * @mixin T
 */
class Wrapper {}
"#;

    let api_uri = "file:///Api.php";
    let api_text = r#"<?php
class Api {
    public function realMethod(): void {}
}
"#;

    let consumer_uri = "file:///Consumer.php";
    let consumer_text = r#"<?php
class Consumer {
    /** @var Wrapper<Api> */
    public $api;

    function test() {
        $this->api->fakeMethod();
    }
}
"#;

    backend.update_ast(wrapper_uri, wrapper_text);
    backend.update_ast(api_uri, api_text);
    backend.update_ast(consumer_uri, consumer_text);

    let diags = unknown_member_diagnostics(&backend, consumer_uri, consumer_text);
    assert!(
        diags.iter().any(|d| d.message.contains("fakeMethod")),
        "fakeMethod does not exist on Api and should be flagged, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// @mixin with template parameter — namespaced (Klaviyo SDK pattern)
// ═══════════════════════════════════════════════════════════════════════════

/// Reproduces the exact Klaviyo SDK pattern with namespaces:
///   - `KlaviyoAPI\Subclient` has `@template TWraps of object` + `@mixin TWraps`
///   - `KlaviyoAPI\KlaviyoAPI` has `/** @var Subclient<EventsApi> */ public $Events;`
///   - A consumer calls `$this->getClient()->Events->createEvent([])`
///
/// The mixin template parameter must resolve through the `@var` generic
/// annotation even when all classes live in different namespaces.
#[test]
fn no_diagnostic_for_mixin_template_param_namespaced_klaviyo_pattern() {
    let backend = create_test_backend();

    let subclient_uri = "file:///vendor/klaviyo/Subclient.php";
    let subclient_text = r#"<?php
namespace KlaviyoAPI;

/**
 * @template TWraps of object
 * @mixin TWraps
 */
class Subclient {
    public function __call(string $name, array $args): mixed {}
}
"#;

    let events_api_uri = "file:///vendor/klaviyo/EventsApi.php";
    let events_api_text = r#"<?php
namespace KlaviyoAPI\API;

class EventsApi {
    public function createEvent(array $body): array {}
    public function getEvents(string $filter): array {}
}
"#;

    let profiles_api_uri = "file:///vendor/klaviyo/ProfilesApi.php";
    let profiles_api_text = r#"<?php
namespace KlaviyoAPI\API;

class ProfilesApi {
    public function getProfiles(?string $additional = null, ?array $fields = null, ?string $filter = null): array {}
    public function updateProfile(string $id, array $body): array {}
}
"#;

    let klaviyo_api_uri = "file:///vendor/klaviyo/KlaviyoAPI.php";
    let klaviyo_api_text = r#"<?php
namespace KlaviyoAPI;

use KlaviyoAPI\API\EventsApi;
use KlaviyoAPI\API\ProfilesApi;

class KlaviyoAPI {
    /** @var Subclient<EventsApi> */
    public $Events;
    /** @var Subclient<ProfilesApi> */
    public $Profiles;
}
"#;

    let service_uri = "file:///src/KlaviyoService.php";
    let service_text = r#"<?php
namespace App\Services;

use KlaviyoAPI\KlaviyoAPI;

class KlaviyoService {
    private ?KlaviyoAPI $client = null;

    private function getClient(): KlaviyoAPI
    {
        return $this->client;
    }

    public function testEvents(): void
    {
        $this->getClient()->Events->createEvent([]);
        $this->getClient()->Events->getEvents('filter');
    }

    public function testProfiles(): void
    {
        $this->getClient()->Profiles->getProfiles(null, ['email'], 'filter');
        $this->getClient()->Profiles->updateProfile('id123', []);
    }
}
"#;

    backend.update_ast(subclient_uri, subclient_text);
    backend.update_ast(events_api_uri, events_api_text);
    backend.update_ast(profiles_api_uri, profiles_api_text);
    backend.update_ast(klaviyo_api_uri, klaviyo_api_text);
    backend.update_ast(service_uri, service_text);

    let diags = unknown_member_diagnostics(&backend, service_uri, service_text);

    assert!(
        !diags.iter().any(|d| d.message.contains("createEvent")),
        "createEvent from mixin TWraps→EventsApi should not be flagged, got: {:?}",
        diags
    );
    assert!(
        !diags.iter().any(|d| d.message.contains("getEvents")),
        "getEvents from mixin TWraps→EventsApi should not be flagged, got: {:?}",
        diags
    );
    assert!(
        !diags.iter().any(|d| d.message.contains("getProfiles")),
        "getProfiles from mixin TWraps→ProfilesApi should not be flagged, got: {:?}",
        diags
    );
    assert!(
        !diags.iter().any(|d| d.message.contains("updateProfile")),
        "updateProfile from mixin TWraps→ProfilesApi should not be flagged, got: {:?}",
        diags
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// B6: Scope methods not found on Builder in analyzer chains
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_diagnostic_for_scope_method_on_builder_in_static_chain() {
    // When a model has scope methods (e.g. scopeWhereIsLuxury), they should be
    // available on the Builder returned by static query methods like
    // whereHas().  The Builder-forwarded methods on the model substitute
    // `static` → `Builder<Model>`, and type_hint_to_classes_typed should
    // inject the model's scope methods onto that Builder.
    let (backend, _dir) = create_psr4_workspace(
        r#"{ "autoload": { "psr-4": { "App\\": "src/", "Illuminate\\": "illuminate/" } } }"#,
        &[
            (
                "illuminate/Database/Eloquent/Model.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

class Model {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Builder.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

/**
 * @template TModel of \Illuminate\Database\Eloquent\Model
 */
class Builder {
    /** @return static */
    public function where(string $column, mixed $operator = null, mixed $value = null): static { return $this; }
    /** @return static */
    public function whereHas(string $relation, ?\Closure $callback = null): static { return $this; }
    /** @return static */
    public function orderBy(string $column, string $direction = 'asc'): static { return $this; }
    /** @return \Illuminate\Database\Eloquent\Collection<int, TModel> */
    public function get(): Collection { return new Collection(); }
    /**
     * @template TValue
     * @param string $column
     * @return \Illuminate\Support\Collection<int, TValue>
     */
    public function pluck(string $column): \Illuminate\Support\Collection { return new \Illuminate\Support\Collection(); }
}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Collection.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

/**
 * @template TKey of array-key
 * @template TModel
 */
class Collection {
    /** @return TModel|null */
    public function first(): mixed { return null; }
}
"#,
            ),
            (
                "illuminate/Support/Collection.php",
                r#"<?php
namespace Illuminate\Support;

/**
 * @template TKey of array-key
 * @template TValue
 */
class Collection {
    /** @return array<TKey, TValue> */
    public function all(): array { return []; }
}
"#,
            ),
            (
                "src/Product.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Builder;

class Product extends Model {
    public function scopeWhereIsLuxury(Builder $query): Builder { return $query->where('is_luxury', true); }
    public function scopeWhereIsDerma(Builder $query): Builder { return $query->where('is_derma', true); }
    public function scopeWhereIsProHairCare(Builder $query): Builder { return $query->where('is_pro_hair_care', true); }
}
"#,
            ),
        ],
    );

    let uri = "file:///consumer.php";
    let text = r#"<?php
use App\Product;

class ProductRepository {
    public function getFiltered(bool $onlyLuxury): void {
        $products = Product::whereHas('translations')
            ->whereIsLuxury()
            ->whereIsDerma()
            ->whereIsProHairCare()
            ->get();
    }
}
"#;
    backend.update_ast(uri, text);
    let mut diags = Vec::new();
    backend.collect_unknown_member_diagnostics(uri, text, &mut diags);

    assert!(
        !diags.iter().any(|d| d.message.contains("whereIsLuxury")),
        "Scope method 'whereIsLuxury' should be found on Builder<Product>, got: {:?}",
        diags
    );
    assert!(
        !diags.iter().any(|d| d.message.contains("whereIsDerma")),
        "Scope method 'whereIsDerma' should be found on Builder<Product>, got: {:?}",
        diags
    );
    assert!(
        !diags
            .iter()
            .any(|d| d.message.contains("whereIsProHairCare")),
        "Scope method 'whereIsProHairCare' should be found on Builder<Product>, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_scope_method_after_wherehas_with_closure() {
    // Same as above but with a closure argument to whereHas, matching
    // the real-world pattern from EventRepository.
    let (backend, _dir) = create_psr4_workspace(
        r#"{ "autoload": { "psr-4": { "App\\": "src/", "Illuminate\\": "illuminate/" } } }"#,
        &[
            (
                "illuminate/Database/Eloquent/Model.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

class Model {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Builder.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

/**
 * @template TModel of \Illuminate\Database\Eloquent\Model
 */
class Builder {
    /** @return static */
    public function where(string $column, mixed $operator = null, mixed $value = null): static { return $this; }
    /**
     * @param  string  $relation
     * @param  (\Closure(\Illuminate\Database\Eloquent\Builder<TModel>): mixed)|null  $callback
     * @return static
     */
    public function whereHas(string $relation, ?\Closure $callback = null): static { return $this; }
    /**
     * @template TValue
     * @param string $column
     * @return \Illuminate\Support\Collection<int, TValue>
     */
    public function pluck(string $column): \Illuminate\Support\Collection { return new \Illuminate\Support\Collection(); }
}
"#,
            ),
            (
                "illuminate/Support/Collection.php",
                r#"<?php
namespace Illuminate\Support;

/**
 * @template TKey of array-key
 * @template TValue
 */
class Collection {
    /** @return array<TKey, TValue> */
    public function all(): array { return []; }
}
"#,
            ),
            (
                "src/Product.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Builder;

class Product extends Model {
    public function scopeWhereIsBlackFriday(Builder $query): Builder { return $query->where('is_black_friday', true); }
    public function scopeWhereIsVisible(Builder $query): Builder { return $query->where('is_visible', true); }
}
"#,
            ),
        ],
    );

    let uri = "file:///consumer.php";
    let text = r#"<?php
use App\Product;
use Illuminate\Database\Eloquent\Builder;

class EventRepository {
    public function getProductIds(): array {
        $ids = Product::whereHas(
            'translations',
            fn(Builder $query): Builder => $query->where('lang_code', 'en')
        )
            ->whereIsBlackFriday()
            ->whereIsVisible()
            ->pluck('id')
            ->all();
        return $ids;
    }
}
"#;
    backend.update_ast(uri, text);
    let mut diags = Vec::new();
    backend.collect_unknown_member_diagnostics(uri, text, &mut diags);

    assert!(
        !diags
            .iter()
            .any(|d| d.message.contains("whereIsBlackFriday")),
        "Scope method 'whereIsBlackFriday' should be found on Builder<Product>, got: {:?}",
        diags
    );
    assert!(
        !diags.iter().any(|d| d.message.contains("whereIsVisible")),
        "Scope method 'whereIsVisible' should be found on Builder<Product>, got: {:?}",
        diags
    );
    // pluck and all should also resolve without issues
    assert!(
        !diags.iter().any(|d| d.message.contains("pluck")),
        "pluck should be found on Builder after scope methods, got: {:?}",
        diags
    );
}

#[test]
fn no_diagnostic_for_scope_in_when_closure_with_callable_inference() {
    // When a closure parameter is typed as bare `Builder` but the
    // enclosing method's callable signature provides `$this`/`static`,
    // the inferred type is refined to `Builder<Product>` (a supertype
    // match with generic args).  Scope methods are then found on the
    // refined type and should NOT be flagged.
    let (backend, _dir) = create_psr4_workspace(
        r#"{ "autoload": { "psr-4": { "App\\": "src/", "Illuminate\\": "illuminate/" } } }"#,
        &[
            (
                "illuminate/Database/Eloquent/Model.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

class Model {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Builder.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

/**
 * @template TModel of \Illuminate\Database\Eloquent\Model
 */
class Builder {
    /** @return static */
    public function where(string $column, mixed $operator = null, mixed $value = null): static { return $this; }
    /** @return static */
    public function whereHas(string $relation, ?\Closure $callback = null): static { return $this; }
    /**
     * @param bool $value
     * @param callable(static): static $callback
     * @return static
     */
    public function when(bool $value, callable $callback): static { return $this; }
    /** @return \Illuminate\Database\Eloquent\Collection<int, TModel> */
    public function get(): Collection { return new Collection(); }

    /** @return mixed */
    public function __call(string $method, array $parameters): mixed { return null; }
}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Collection.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

/**
 * @template TKey of array-key
 * @template TModel
 */
class Collection {}
"#,
            ),
            (
                "src/Product.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Builder;

class Product extends Model {
    public function scopeWhereIsLuxury(Builder $query): Builder { return $query->where('is_luxury', true); }
    public function scopeWhereIsDerma(Builder $query): Builder { return $query->where('is_derma', true); }
}
"#,
            ),
        ],
    );

    let uri = "file:///consumer.php";
    let text = r#"<?php
use App\Product;
use Illuminate\Database\Eloquent\Builder;

class ProductRepository {
    public function getFiltered(bool $onlyLuxury, bool $onlyDerma): void {
        Product::whereHas('translations')
            ->when($onlyLuxury, fn(Builder $q): Builder => $q->whereIsLuxury())
            ->when($onlyDerma, fn(Builder $q): Builder => $q->whereIsDerma())
            ->get();
    }
}
"#;
    backend.update_ast(uri, text);
    let mut diags = Vec::new();
    backend.collect_unknown_member_diagnostics(uri, text, &mut diags);

    // The callable signature `callable(static)` on `when()` provides
    // `static` as the closure param type.  Since the receiver is
    // `Builder<Product>`, `static` resolves to `Builder<Product>`.
    // The explicit `Builder` type hint is a supertype, so the inferred
    // `Builder<Product>` is preferred — scope methods are found.
    assert!(
        !diags.iter().any(|d| d.message.contains("whereIsLuxury")),
        "Scope method should be found via callable param inference from when(), got: {:?}",
        diags
    );
    assert!(
        !diags.iter().any(|d| d.message.contains("whereIsDerma")),
        "Scope method should be found via callable param inference from when(), got: {:?}",
        diags
    );

    // Known methods after the scope calls should also resolve.
    assert!(
        !diags.iter().any(|d| d.message.contains("get")),
        "Known method 'get' should resolve after scope calls, got: {:?}",
        diags
    );
    // No broken-chain / unresolved diagnostics downstream.
    assert!(
        !diags
            .iter()
            .any(|d| d.message.contains("could not be resolved")),
        "Chain should not break, got: {:?}",
        diags
    );
}

#[test]
fn scope_on_standalone_bare_builder_param_flags_warning_chain_continues() {
    // When a function parameter is typed as bare `Builder` (no callable
    // inference context), scope methods cannot be verified statically.
    // They are flagged via MagicFallback (__call exists), but the chain
    // continues because Builder's __call return type is patched to
    // `static` during resolution.
    let (backend, _dir) = create_psr4_workspace(
        r#"{ "autoload": { "psr-4": { "App\\": "src/", "Illuminate\\": "illuminate/" } } }"#,
        &[
            (
                "illuminate/Database/Eloquent/Model.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

class Model {}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Builder.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

/**
 * @template TModel of \Illuminate\Database\Eloquent\Model
 */
class Builder {
    /** @return static */
    public function where(string $column, mixed $operator = null, mixed $value = null): static { return $this; }
    /** @return static */
    public function orderBy(string $column, string $direction = 'asc'): static { return $this; }
    /** @return \Illuminate\Database\Eloquent\Collection<int, TModel> */
    public function get(): Collection { return new Collection(); }

    /** @return mixed */
    public function __call(string $method, array $parameters): mixed { return null; }
}
"#,
            ),
            (
                "illuminate/Database/Eloquent/Collection.php",
                r#"<?php
namespace Illuminate\Database\Eloquent;

/**
 * @template TKey of array-key
 * @template TModel
 */
class Collection {}
"#,
            ),
            (
                "src/Product.php",
                r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Builder;

class Product extends Model {
    public function scopeWhereIsLuxury(Builder $query): Builder { return $query->where('is_luxury', true); }
}
"#,
            ),
        ],
    );

    let uri = "file:///consumer.php";
    // Standalone function parameter — no callable inference context.
    let text = r#"<?php
use Illuminate\Database\Eloquent\Builder;

function filterProducts(Builder $query): void {
    $query->whereIsLuxury()->orderBy('name')->get();
}
"#;
    backend.update_ast(uri, text);
    let mut diags = Vec::new();
    backend.collect_unknown_member_diagnostics(uri, text, &mut diags);

    // Scope method IS flagged — no callable inference to refine the
    // bare Builder to Builder<Product>.
    assert!(
        diags.iter().any(|d| d.message.contains("whereIsLuxury")),
        "Scope method on standalone bare Builder param should be flagged, got: {:?}",
        diags
    );

    // Chain continues — known methods after the unknown scope call
    // should NOT be flagged because __call returns static.
    assert!(
        !diags.iter().any(|d| d.message.contains("orderBy")),
        "Known method 'orderBy' should resolve after __call fallback, got: {:?}",
        diags
    );
    assert!(
        !diags.iter().any(|d| d.message.contains("get")),
        "Known method 'get' should resolve after __call fallback, got: {:?}",
        diags
    );
    assert!(
        !diags
            .iter()
            .any(|d| d.message.contains("could not be resolved")),
        "Chain should not break after __call fallback, got: {:?}",
        diags
    );
}

/// Cross-file variant of B12: `Collection::reduce()` loaded via PSR-4 with
/// two method-level `@template` params and a `callable(TReduceInitial|TReduceReturnType, TValue, TKey): TReduceReturnType`
/// parameter.  The return type `TReduceReturnType` must be inferred from the
/// closure's return type annotation even when the Collection class lives in
/// a separate file.
#[test]
fn no_false_positive_on_reduce_two_tpl_cross_file() {
    let composer = r#"{"autoload":{"psr-4":{"App\\":"src/","Illuminate\\Support\\":"vendor/illuminate/support/src/"}}}"#;

    let collection_php = r#"<?php
namespace Illuminate\Support;

/**
 * @template TKey
 * @template TValue
 */
class Collection {
    /**
     * @template TReduceInitial
     * @template TReduceReturnType
     * @param callable(TReduceInitial|TReduceReturnType, TValue, TKey): TReduceReturnType $callback
     * @param TReduceInitial $initial
     * @return TReduceInitial|TReduceReturnType
     */
    public function reduce(callable $callback, mixed $initial = null): mixed {}
}
"#;

    let decimal_php = r#"<?php
namespace App;

class Decimal {
    public function add(Decimal $other): Decimal { return $this; }
    public function getValue(): string { return '0'; }
}
"#;

    let order_product_php = r#"<?php
namespace App;

class OrderProduct {
    public float $price;
}
"#;

    let service_php = r#"<?php
namespace App;

use Illuminate\Support\Collection;

class FlowService {
    public function test(): void {
        /** @var Collection<int, OrderProduct> $products */
        $products = new Collection();
        $products->reduce(fn(Decimal $c, OrderProduct $p): Decimal => $c->add($p->price), new Decimal('0'))->add(new Decimal('1'));
        $products->reduce(fn(Decimal $c, OrderProduct $p): Decimal => $c->add($p->price), new Decimal('0'))->getValue();
    }
}
"#;

    let (backend, _dir) = create_psr4_workspace(
        composer,
        &[
            (
                "vendor/illuminate/support/src/Collection.php",
                collection_php,
            ),
            ("src/Decimal.php", decimal_php),
            ("src/OrderProduct.php", order_product_php),
            ("src/FlowService.php", service_php),
        ],
    );

    let uri = &format!(
        "file://{}",
        _dir.path().join("src/FlowService.php").display()
    );
    let diags = unknown_member_diagnostics(&backend, uri, service_php);

    let chained_diags: Vec<_> = diags.iter().filter(|d| !d.message.contains("$c")).collect();
    assert!(
        !chained_diags.iter().any(|d| d.message.contains("add")),
        "reduce() should resolve TReduceReturnType=Decimal cross-file, chained 'add' should be known, got: {:?}",
        chained_diags
    );
    assert!(
        !chained_diags.iter().any(|d| d.message.contains("getValue")),
        "reduce() should resolve TReduceReturnType=Decimal cross-file, chained 'getValue' should be known, got: {:?}",
        chained_diags
    );
    assert!(
        !chained_diags
            .iter()
            .any(|d| d.message.contains("could not be resolved")),
        "reduce() return type should be fully resolved cross-file when chained, got: {:?}",
        chained_diags
    );
}

/// Cross-file test modelling the real Laravel structure: `Collection` uses
/// a trait `EnumeratesValues` (which defines `reduce()` with
/// `@return TReduceReturnType`) and implements an interface `Enumerable`
/// (which declares `reduce()` with `@return TReduceInitial|TReduceReturnType`).
/// The inheritance merger might pick up the interface's union return type,
/// so the template substitution must handle both template params in the
/// return type union.
///
/// Regression test for B12.
#[test]
fn no_false_positive_on_reduce_trait_interface_pattern() {
    let composer = r#"{"autoload":{"psr-4":{"App\\":"src/","Illuminate\\Support\\":"vendor/illuminate/support/src/"}}}"#;

    let enumerable_php = r#"<?php
namespace Illuminate\Support;

/**
 * @template TKey
 * @template TValue
 */
interface Enumerable {
    /**
     * @template TReduceInitial
     * @template TReduceReturnType
     * @param callable(TReduceInitial|TReduceReturnType, TValue, TKey): TReduceReturnType $callback
     * @param TReduceInitial $initial
     * @return TReduceInitial|TReduceReturnType
     */
    public function reduce(callable $callback, $initial = null);
}
"#;

    let trait_php = r#"<?php
namespace Illuminate\Support;

trait EnumeratesValues {
    /**
     * @template TReduceInitial
     * @template TReduceReturnType
     * @param callable(TReduceInitial|TReduceReturnType, TValue, TKey): TReduceReturnType $callback
     * @param TReduceInitial $initial
     * @return TReduceReturnType
     */
    public function reduce(callable $callback, $initial = null)
    {
        $result = $initial;
        foreach ($this as $key => $value) {
            $result = $callback($result, $value, $key);
        }
        return $result;
    }
}
"#;

    let collection_php = r#"<?php
namespace Illuminate\Support;

/**
 * @template TKey
 * @template TValue
 * @implements Enumerable<TKey, TValue>
 */
class Collection implements Enumerable {
    use EnumeratesValues;
}
"#;

    let decimal_php = r#"<?php
namespace App;

class Decimal {
    public function add(Decimal $other): Decimal { return $this; }
    public function getValue(): string { return '0'; }
}
"#;

    let order_product_php = r#"<?php
namespace App;

class OrderProduct {
    public float $price;
}
"#;

    let service_php = r#"<?php
namespace App;

use Illuminate\Support\Collection;

class FlowService {
    public function test(): void {
        /** @var Collection<int, OrderProduct> $products */
        $products = new Collection();
        $products->reduce(fn(Decimal $c, OrderProduct $p): Decimal => $c->add($p->price), new Decimal('0'))->add(new Decimal('1'));
        $products->reduce(fn(Decimal $c, OrderProduct $p): Decimal => $c->add($p->price), new Decimal('0'))->getValue();
    }
}
"#;

    let (backend, _dir) = create_psr4_workspace(
        composer,
        &[
            (
                "vendor/illuminate/support/src/Enumerable.php",
                enumerable_php,
            ),
            (
                "vendor/illuminate/support/src/EnumeratesValues.php",
                trait_php,
            ),
            (
                "vendor/illuminate/support/src/Collection.php",
                collection_php,
            ),
            ("src/Decimal.php", decimal_php),
            ("src/OrderProduct.php", order_product_php),
            ("src/FlowService.php", service_php),
        ],
    );

    let uri = &format!(
        "file://{}",
        _dir.path().join("src/FlowService.php").display()
    );
    let diags = unknown_member_diagnostics(&backend, uri, service_php);

    let chained_diags: Vec<_> = diags.iter().filter(|d| !d.message.contains("$c")).collect();
    assert!(
        !chained_diags.iter().any(|d| d.message.contains("add")),
        "reduce() via trait+interface should resolve TReduceReturnType=Decimal, chained 'add' should be known, got: {:?}",
        chained_diags
    );
    assert!(
        !chained_diags.iter().any(|d| d.message.contains("getValue")),
        "reduce() via trait+interface should resolve TReduceReturnType=Decimal, chained 'getValue' should be known, got: {:?}",
        chained_diags
    );
    assert!(
        !chained_diags
            .iter()
            .any(|d| d.message.contains("could not be resolved")),
        "reduce() return type via trait+interface should be fully resolved when chained, got: {:?}",
        chained_diags
    );
}

/// `Collection::reduce()` with two method-level `@template` params
/// (`TReduceInitial`, `TReduceReturnType`) and a callable whose first
/// parameter is the union `TReduceInitial|TReduceReturnType`.  The
/// return type is `TReduceReturnType` which should be inferred from
/// the closure's return type annotation.  Chaining `.add()` on the
/// result must not produce a diagnostic.
///
/// Regression test for B12.
#[test]
fn no_false_positive_on_reduce_with_two_template_params() {
    let backend = create_test_backend();
    let uri = "file:///test_reduce_b12.php";
    let text = r#"<?php
class Decimal {
    public function add(Decimal $other): Decimal { return $this; }
    public function getValue(): string { return '0'; }
}

class OrderProduct {
    public float $price;
}

/**
 * @template TKey
 * @template TValue
 */
class Collection {
    /**
     * @template TReduceInitial
     * @template TReduceReturnType
     * @param callable(TReduceInitial|TReduceReturnType, TValue, TKey): TReduceReturnType $callback
     * @param TReduceInitial $initial
     * @return TReduceReturnType
     */
    public function reduce(callable $callback, mixed $initial = null): mixed {}
}

class FlowService {
    public function test(): void {
        /** @var Collection<int, OrderProduct> $products */
        $products = new Collection();
        $products->reduce(fn(Decimal $c, OrderProduct $p): Decimal => $c->add($p->price), new Decimal('0'))->add(new Decimal('1'));
        $products->reduce(fn(Decimal $c, OrderProduct $p): Decimal => $c->add($p->price), new Decimal('0'))->getValue();
    }
}
"#;
    let diags = unknown_member_diagnostics(&backend, uri, text);
    // Filter out diagnostics for the inner `$c->add($p->price)` inside
    // the closure — we only care about the chained call after reduce().
    let chained_diags: Vec<_> = diags.iter().filter(|d| !d.message.contains("$c")).collect();
    assert!(
        !chained_diags.iter().any(|d| d.message.contains("add")),
        "reduce() should resolve TReduceReturnType=Decimal, chained 'add' should be known, got: {:?}",
        chained_diags
    );
    assert!(
        !chained_diags.iter().any(|d| d.message.contains("getValue")),
        "reduce() should resolve TReduceReturnType=Decimal, chained 'getValue' should be known, got: {:?}",
        chained_diags
    );
    assert!(
        !chained_diags
            .iter()
            .any(|d| d.message.contains("could not be resolved")),
        "reduce() return type should be fully resolved when chained, got: {:?}",
        chained_diags
    );
}
