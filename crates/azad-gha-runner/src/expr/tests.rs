#![allow(clippy::unwrap_used)]

use serde_json::{Value, json};

use crate::expr::*;

struct Test {
    data: Value,
    status: JobStatus,
}

impl Context for Test {
    fn context(&self, name: &str) -> Option<Value> {
        self.data.get(name).cloned()
    }

    fn job_status(&self) -> JobStatus {
        self.status
    }

    fn hash_files(&self, patterns: &[String]) -> Result<String, String> {
        match patterns.first().map(String::as_str) {
            Some("fail") => Err("no such file".to_string()),
            _ => Ok(format!("hash:{}", patterns.join("|"))),
        }
    }
}

fn data() -> Value {
    json!({
        "github": {
            "event_name": "push",
            "ref": "refs/heads/main",
            "event": {"issue": {"labels": [{"name": "bug", "id": 1}, {"name": "help", "id": 2}, {"id": 3}]}},
            "actor": "Octocat",
            "run_number": 7
        },
        "env": {"Name": "value", "empty": ""},
        "matrix": {"os": "linux", "node": 18, "list": [1, 2, 3]},
        "steps": {"build": {"outputs": {"json": "{\"a\": [1, 2]}"}}},
        "needs": {}
    })
}

fn with_status(status: JobStatus) -> Test {
    Test { data: data(), status }
}

fn ctx() -> Test {
    with_status(JobStatus::Success)
}

fn eval(source: &str) -> Value {
    evaluate(&parse(source).unwrap(), &ctx()).unwrap()
}

fn eval_error(source: &str) -> String {
    evaluate(&parse(source).unwrap(), &ctx()).unwrap_err().to_string()
}

fn text(source: &str) -> String {
    to_text(&eval(source))
}

#[test]
fn literals() {
    assert_eq!(eval("null"), Value::Null);
    assert_eq!(eval("true"), json!(true));
    assert_eq!(eval("false"), json!(false));
    assert_eq!(eval("711"), json!(711));
    assert_eq!(eval("-9.2"), json!(-9.2));
    assert_eq!(eval("0xff"), json!(255));
    assert_eq!(eval("-0x10"), json!(-16));
    assert_eq!(eval("1.5e3"), json!(1500));
    assert_eq!(eval("2e-2"), json!(0.02));
    assert_eq!(eval("'it''s'"), json!("it's"));
    assert_eq!(eval("''"), json!(""));
}

#[test]
fn github_documentation_examples() {
    assert_eq!(eval("contains('Hello world', 'llo')"), json!(true));
    assert_eq!(eval("contains('Hello world', 'LLO')"), json!(true));
    assert_eq!(eval("startsWith('Hello world', 'He')"), json!(true));
    assert_eq!(eval("endsWith('Hello world', 'ld')"), json!(true));
    assert_eq!(text("format('Hello {0} {1} {2}!', 'Mona', 'the', 'Octocat')"), "Hello Mona the Octocat!");
    assert_eq!(text("format('{{Hello {0} {1} {2}!}}', 'Mona', 'the', 'Octocat')"), "{Hello Mona the Octocat!}");
    assert_eq!(text("join(matrix.list, ', ')"), "1, 2, 3");
    assert_eq!(text("join(matrix.list)"), "1,2,3");
    assert_eq!(text("fromJSON(steps.build.outputs.json).a[1]"), "2");
    assert_eq!(eval("contains(github.event.issue.labels.*.name, 'bug')"), json!(true));
    assert_eq!(eval("contains(github.event.issue.labels.*.name, 'nope')"), json!(false));
    assert_eq!(
        text("hashFiles('**/package-lock.json', '**/Gemfile.lock')"),
        "hash:**/package-lock.json|**/Gemfile.lock"
    );
}

#[test]
fn precedence() {
    assert_eq!(eval("true || false && false"), json!(true));
    assert_eq!(eval("(true || false) && false"), json!(false));
    assert_eq!(eval("!false == true"), json!(true));
    assert_eq!(eval("1 < 2 == true"), json!(true));
    assert_eq!(eval("1 == 1 && 2 > 1"), json!(true));
    assert_eq!(eval("!(1 == 1)"), json!(false));
    assert_eq!(eval("false && true || true"), json!(true));
}

#[test]
fn and_or_return_operands() {
    assert_eq!(eval("'a' && 'b'"), json!("b"));
    assert_eq!(eval("'' && 'b'"), json!(""));
    assert_eq!(eval("null || 'fallback'"), json!("fallback"));
    assert_eq!(eval("'first' || 'second'"), json!("first"));
    assert_eq!(eval("0 || null"), Value::Null);
    assert_eq!(eval("matrix.os == 'linux' && 'yes' || 'no'"), json!("yes"));
}

#[test]
fn and_or_do_not_evaluate_the_unused_side() {
    assert_eq!(eval("true || missing.value"), json!(true));
    assert_eq!(eval("false && missing.value"), json!(false));
    assert!(eval_error("true && missing.value").contains("unknown context `missing`"));
}

#[test]
fn names_are_case_insensitive() {
    assert_eq!(text("GITHUB.Event_Name"), "push");
    assert_eq!(text("github.EVENT_NAME"), "push");
    assert_eq!(text("env.name"), "value");
    assert_eq!(text("github['Event_name']"), "push");
    assert_eq!(eval("CONTAINS('abc', 'B')"), json!(true));
    assert_eq!(eval("Success()"), json!(true));
}

#[test]
fn access_on_null_and_missing_gives_null() {
    assert_eq!(eval("github.nothing.deeper.still"), Value::Null);
    assert_eq!(eval("null.x"), Value::Null);
    assert_eq!(eval("matrix.list[10]"), Value::Null);
    assert_eq!(eval("matrix.os.length"), Value::Null);
    assert_eq!(eval("matrix.list['x']"), Value::Null);
}

#[test]
fn index_access() {
    assert_eq!(eval("matrix.list[0]"), json!(1));
    assert_eq!(eval("matrix.list[1]"), json!(2));
    assert_eq!(eval("matrix.list['1']"), json!(2));
    assert_eq!(eval("matrix['os']"), json!("linux"));
    assert_eq!(eval("matrix[env.empty]"), Value::Null);
}

#[test]
fn object_filter() {
    assert_eq!(eval("github.event.issue.labels.*.name"), json!(["bug", "help"]));
    assert_eq!(eval("github.event.issue.labels[*].id"), json!([1, 2, 3]));
    assert_eq!(eval("matrix.list.*"), json!([1, 2, 3]));
    assert_eq!(eval("github.event_name.*"), json!([]));
    assert_eq!(eval("env.*.length"), json!([]));
    assert_eq!(eval("github.event.*.labels.*.name"), json!(["bug", "help"]));
    assert_eq!(text("join(github.event.issue.labels.*.name, '/')"), "bug/help");
}

#[test]
fn filter_over_objects_gives_their_values() {
    assert_eq!(eval("matrix.*.os"), json!([]));
    assert_eq!(
        eval("github.event.*.labels"),
        json!([[{"name": "bug", "id": 1}, {"name": "help", "id": 2}, {"id": 3}]])
    );
}

#[test]
fn loose_equality_across_types() {
    assert_eq!(eval("null == 0"), json!(true));
    assert_eq!(eval("null == ''"), json!(true));
    assert_eq!(eval("null == false"), json!(true));
    assert_eq!(eval("true == 1"), json!(true));
    assert_eq!(eval("false == 0"), json!(true));
    assert_eq!(eval("'1' == 1"), json!(true));
    assert_eq!(eval("'1.0' == 1"), json!(true));
    assert_eq!(eval("'0x10' == 16"), json!(true));
    assert_eq!(eval("'abc' == 0"), json!(false));
    assert_eq!(eval("'abc' != 0"), json!(true));
    assert_eq!(eval("'ABC' == 'abc'"), json!(true));
    assert_eq!(eval("'abc' != 'ABC'"), json!(false));
    assert_eq!(eval("null == null"), json!(true));
}

#[test]
fn arrays_and_objects_are_never_equal() {
    assert_eq!(eval("matrix.list == matrix.list"), json!(false));
    assert_eq!(eval("matrix == matrix"), json!(false));
    assert_eq!(eval("matrix.list != matrix.list"), json!(true));
    assert_eq!(eval("matrix.list == 1"), json!(false));
}

#[test]
fn comparison() {
    assert_eq!(eval("1 < 2"), json!(true));
    assert_eq!(eval("2 <= 2"), json!(true));
    assert_eq!(eval("3 > 2"), json!(true));
    assert_eq!(eval("2 >= 3"), json!(false));
    assert_eq!(eval("'a' < 'B'"), json!(true));
    assert_eq!(eval("'10' > 9"), json!(true));
    assert_eq!(eval("'abc' < 1"), json!(false));
    assert_eq!(eval("'abc' >= 1"), json!(false));
    assert_eq!(eval("matrix.list < 5"), json!(false));
    assert_eq!(eval("null <= 0"), json!(true));
    assert_eq!(eval("true > false"), json!(true));
}

#[test]
fn truthiness() {
    for falsy in [json!(null), json!(false), json!(0), json!(-0.0), json!("")] {
        assert!(!truthy(&falsy), "{falsy}");
    }
    for truthy_value in [json!(true), json!(1), json!(-1), json!("0"), json!("false"), json!([]), json!({})] {
        assert!(truthy(&truthy_value), "{truthy_value}");
    }
    assert_eq!(eval("!''"), json!(true));
    assert_eq!(eval("!'x'"), json!(false));
    assert_eq!(eval("!!matrix.list"), json!(true));
}

#[test]
fn number_text() {
    assert_eq!(to_text(&json!(1)), "1");
    assert_eq!(to_text(&json!(1.0)), "1");
    assert_eq!(to_text(&json!(-12)), "-12");
    assert_eq!(to_text(&json!(0.5)), "0.5");
    assert_eq!(to_text(&json!(0.1 + 0.2)), "0.3");
    assert_eq!(to_text(&json!(123456789012345_i64)), "123456789012345");
    assert_eq!(to_text(&json!(1e15)), "1E+15");
    assert_eq!(to_text(&json!(1.5e20)), "1.5E+20");
    assert_eq!(to_text(&json!(0.0001)), "0.0001");
    assert_eq!(to_text(&json!(0.00001)), "1E-05");
    assert_eq!(to_text(&json!(-0.0)), "-0");
    assert_eq!(text("100"), "100");
    assert_eq!(text("0.25"), "0.25");
}

#[test]
fn text_of_other_values() {
    assert_eq!(to_text(&json!(null)), "");
    assert_eq!(to_text(&json!(true)), "true");
    assert_eq!(to_text(&json!(false)), "false");
    assert_eq!(to_text(&json!({"a": 1})), "Object");
    assert_eq!(to_text(&json!([1])), "Array");
}

#[test]
fn contains_variants() {
    assert_eq!(eval("contains(matrix.list, 2)"), json!(true));
    assert_eq!(eval("contains(matrix.list, '2')"), json!(true));
    assert_eq!(eval("contains(matrix.list, 9)"), json!(false));
    assert_eq!(eval("contains('abc123', 123)"), json!(true));
    assert_eq!(eval("contains(null, '')"), json!(true));
    assert_eq!(eval("contains(matrix, 'os')"), json!(false));
    assert_eq!(eval("contains('abc', matrix.list)"), json!(false));
}

#[test]
fn starts_and_ends_with_are_case_insensitive() {
    assert_eq!(eval("startsWith('Hello', 'hE')"), json!(true));
    assert_eq!(eval("endsWith('Hello', 'LO')"), json!(true));
    assert_eq!(eval("startsWith('Hello', 'x')"), json!(false));
    assert_eq!(eval("startsWith(matrix.list, '1')"), json!(false));
}

#[test]
fn format_escapes_and_errors() {
    assert_eq!(text("format('{{0}}')"), "{0}");
    assert_eq!(text("format('{0}{0}', 'a')"), "aa");
    assert_eq!(text("format('{1} {0}', 'a', 'b')"), "b a");
    assert_eq!(text("format('{0}-{1}', 1, true)"), "1-true");
    assert_eq!(text("format('a}}b{{c')"), "a}b{c");
    assert!(eval_error("format('{0} {1}', 'a')").contains("{1} has no argument"));
    assert!(eval_error("format('{')").contains("not `{` number `}`"));
    assert!(eval_error("format('}')").contains("no matching"));
    assert!(eval_error("format('{x}', 1)").contains("not `{` number `}`"));
}

#[test]
fn join_variants() {
    assert_eq!(text("join('abc', ',')"), "abc");
    assert_eq!(text("join(github.event.issue.labels.*.name, ' and ')"), "bug and help");
    assert_eq!(text("join(matrix.list, 0)"), "10203");
    assert_eq!(text("join(matrix.list, '')"), "123");
}

#[test]
fn to_json_prints_with_two_space_indent() {
    assert_eq!(text("toJSON(matrix.list)"), "[\n  1,\n  2,\n  3\n]");
    assert_eq!(text("toJSON(1)"), "1");
    assert_eq!(text("toJSON('a')"), "\"a\"");
    assert_eq!(text("toJSON(null)"), "null");
    assert_eq!(text("toJSON(env)"), "{\n  \"Name\": \"value\",\n  \"empty\": \"\"\n}");
}

#[test]
fn from_json_parses_and_rejects() {
    assert_eq!(eval("fromJSON('[1, \"a\", null]')"), json!([1, "a", null]));
    assert_eq!(eval("fromJSON('true')"), json!(true));
    assert_eq!(eval("fromJSON('{\"a\": {\"b\": 5}}').a.b"), json!(5));
    assert!(eval_error("fromJSON('{')").contains("cannot read"));
}

#[test]
fn hash_files_reports_context_errors() {
    assert!(eval_error("hashFiles('fail')").contains("no such file"));
}

#[test]
fn status_functions_read_the_job_status() {
    let failed = with_status(JobStatus::Failure);
    let cancelled = with_status(JobStatus::Cancelled);
    let run = |source: &str, context: &Test| evaluate(&parse(source).unwrap(), context).unwrap();
    assert_eq!(run("success()", &ctx()), json!(true));
    assert_eq!(run("success()", &failed), json!(false));
    assert_eq!(run("failure()", &failed), json!(true));
    assert_eq!(run("cancelled()", &cancelled), json!(true));
    assert_eq!(run("always()", &cancelled), json!(true));
    assert_eq!(run("failure()", &ctx()), json!(false));
}

#[test]
fn errors_are_clear() {
    assert!(eval_error("nope(1)").contains("unknown function `nope`"));
    assert!(eval_error("contains('a')").contains("takes 2 argument(s), but 1 given"));
    assert!(eval_error("join()").contains("takes 1 to 2"));
    assert!(eval_error("success(1)").contains("takes 0"));
    assert!(eval_error("format()").contains("at least 1"));
    assert!(eval_error("unknown.thing").contains("unknown context `unknown`"));
    assert!(eval_error("unknown.thing").contains("`unknown.thing`"));
}

fn parse_error(source: &str) -> ParseError {
    parse(source).unwrap_err()
}

#[test]
fn parse_errors_name_the_position() {
    let error = parse_error("1 + 2");
    assert_eq!(error.position, 3);
    assert!(error.to_string().contains("position 3"));
    assert!(error.to_string().contains("`1 + 2`"));
    assert_eq!(parse_error("'abc").position, 1);
    assert!(parse_error("(1").message.contains("expected `)`"));
    assert!(parse_error("a.").message.contains("ends too early"));
    assert!(parse_error("").message.contains("ends too early"));
    assert!(parse_error("a = b").message.contains("expected `==`"));
    assert!(parse_error("a & b").message.contains("expected `&&`"));
    assert!(parse_error("1.2.3").message.contains("not a valid number"));
    assert!(parse_error("f(1,)").message.contains("expected a value"));
    assert!(parse_error("a b").message.contains("unexpected token"));
    assert!(parse_error("a[1").message.contains("expected `]`"));
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    let source = format!("{}1{}", "(".repeat(500), ")".repeat(500));
    assert!(parse_error(&source).message.contains("nested too deeply"));
    let nots = format!("{}1", "!".repeat(500));
    assert!(parse_error(&nots).message.contains("nested too deeply"));
}

#[test]
fn names_may_contain_dashes_and_digits() {
    let context = Test { data: json!({"needs": {"job-1": {"outputs": {"out_put": "x"}}}}), status: JobStatus::Success };
    let expr = parse("needs.job-1.outputs.out_put").unwrap();
    assert_eq!(evaluate(&expr, &context).unwrap(), json!("x"));
}

#[test]
fn interpolate_several_parts_with_text() {
    let context = ctx();
    assert_eq!(
        interpolate("a ${{ matrix.os }}-${{ matrix.node }} b ${{ github.run_number }}!", &context).unwrap(),
        "a linux-18 b 7!"
    );
    assert_eq!(interpolate("no expressions", &context).unwrap(), "no expressions");
    assert_eq!(interpolate("${{ null }}x", &context).unwrap(), "x");
    assert_eq!(interpolate("${{matrix.list}}", &context).unwrap(), "Array");
    assert_eq!(interpolate("héllo ${{ 'wörld' }} ✓", &context).unwrap(), "héllo wörld ✓");
}

#[test]
fn interpolate_keeps_close_braces_inside_strings() {
    let context = ctx();
    assert_eq!(interpolate("${{ '}}' }}", &context).unwrap(), "}}");
    assert_eq!(interpolate("x ${{ format('{{{0}}}', 'a') }} y", &context).unwrap(), "x {a} y");
    assert_eq!(interpolate("${{ 'it''s }} ok' }}", &context).unwrap(), "it's }} ok");
}

#[test]
fn interpolate_errors() {
    let context = ctx();
    let unclosed = interpolate("abc ${{ matrix.os", &context).unwrap_err();
    assert!(matches!(&unclosed, Error::Template(error) if error.position == 5));
    assert!(unclosed.to_string().contains("no closing"));
    assert!(matches!(interpolate("${{ 1 + }}", &context), Err(Error::Parse(_))));
    let eval_failure = interpolate("${{ nope.x }}", &context).unwrap_err();
    assert!(matches!(eval_failure, Error::Eval(_)));
    assert!(eval_failure.to_string().contains("nope.x"));
}

#[test]
fn evaluate_template_keeps_the_type_of_a_single_expression() {
    let context = ctx();
    assert_eq!(evaluate_template("${{ matrix.list }}", &context).unwrap(), json!([1, 2, 3]));
    assert_eq!(evaluate_template("${{ matrix.node }}", &context).unwrap(), json!(18));
    assert_eq!(evaluate_template("${{ true }}", &context).unwrap(), json!(true));
    assert_eq!(evaluate_template("${{ matrix.node }} ", &context).unwrap(), json!("18 "));
    assert_eq!(evaluate_template("n=${{ matrix.node }}", &context).unwrap(), json!("n=18"));
    assert_eq!(evaluate_template("${{ 1 }}${{ 2 }}", &context).unwrap(), json!("12"));
    assert_eq!(evaluate_template("plain", &context).unwrap(), json!("plain"));
}

fn check(source: &str, status: JobStatus) -> bool {
    condition(source, &with_status(status)).unwrap()
}

#[test]
fn condition_adds_the_implicit_success() {
    assert!(check("true", JobStatus::Success));
    assert!(!check("true", JobStatus::Failure));
    assert!(!check("true", JobStatus::Cancelled));
    assert!(!check("github.event_name == 'push'", JobStatus::Failure));
    assert!(check("github.event_name == 'push'", JobStatus::Success));
    assert!(!check("false", JobStatus::Success));
}

#[test]
fn condition_with_status_function_has_no_implicit_success() {
    assert!(check("failure()", JobStatus::Failure));
    assert!(!check("failure()", JobStatus::Success));
    assert!(check("always()", JobStatus::Cancelled));
    assert!(check("cancelled() && github.event_name == 'push'", JobStatus::Cancelled));
    assert!(check("success() || failure()", JobStatus::Failure));
    assert!(check("always() && !contains(github.ref, 'x')", JobStatus::Failure));
}

#[test]
fn condition_accepts_delimiters_and_whitespace() {
    assert!(check("${{ github.event_name == 'push' }}", JobStatus::Success));
    assert!(check("  ${{ always() }}  ", JobStatus::Failure));
    assert!(check("  matrix.os == 'linux'  ", JobStatus::Success));
}

#[test]
fn condition_does_not_evaluate_when_the_job_failed() {
    assert!(!check("missing.value", JobStatus::Failure));
    assert!(condition("missing.value", &ctx()).is_err());
}

#[test]
fn condition_rejects_text_mixed_with_expressions() {
    let error = condition("${{ true }} == true", &ctx()).unwrap_err();
    assert!(matches!(error, Error::Template(_)));
}

#[test]
fn references_lists_contexts_and_special_functions() {
    let refs = references(&parse("matrix.os == 'linux' && contains(Steps.build.outputs.x, github.ref)").unwrap());
    let names: Vec<&str> = refs.contexts.iter().map(String::as_str).collect();
    assert_eq!(names, ["github", "matrix", "steps"]);
    assert!(!refs.status_function);
    assert!(!refs.hash_files);

    let refs = references(&parse("!cancelled() && hashFiles('a') != ''").unwrap());
    assert!(refs.contexts.is_empty());
    assert!(refs.status_function);
    assert!(refs.hash_files);

    for source in ["success()", "failure()", "always()", "cancelled()"] {
        assert!(references(&parse(source).unwrap()).status_function, "{source}");
    }
}

#[test]
fn references_see_every_part_of_an_expression() {
    let refs = references(&parse("a[b.c] || !d.*.e").unwrap());
    let names: Vec<&str> = refs.contexts.iter().map(String::as_str).collect();
    assert_eq!(names, ["a", "b", "d"]);
}

#[test]
fn template_expressions_lists_each_part() {
    let expressions = template_expressions("a ${{ matrix.os }} b ${{ env.x }}").unwrap();
    let sources: Vec<&str> = expressions.iter().map(Expr::source).collect();
    assert_eq!(sources, [" matrix.os ", " env.x "]);
    assert!(template_expressions("none").unwrap().is_empty());
    assert!(template_expressions("${{").is_err());
}
