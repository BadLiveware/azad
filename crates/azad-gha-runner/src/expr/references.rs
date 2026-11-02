use crate::expr::eval::References;
use crate::expr::functions::{FN_ALWAYS, FN_CANCELLED, FN_FAILURE, FN_HASH_FILES, FN_SUCCESS};
use crate::expr::parser::{Expr, Node};

/// The names of the contexts that an expression reads (such as `matrix` and `steps`), and whether it calls a status
/// function. A compiler uses this to decide whether an expression can be evaluated before a job runs.
/// The context names are in lower case.
pub fn references(expr: &Expr) -> References {
    let mut found = References::default();
    visit(&expr.root, &mut found);
    found
}

fn visit(node: &Node, found: &mut References) {
    match node {
        Node::Literal(_) => {}
        Node::Context(name) => {
            found.contexts.insert(name.clone());
        }
        Node::Property(target, _) | Node::Filter(target) | Node::Not(target) => visit(target, found),
        Node::Index(left, right) | Node::Compare(_, left, right) | Node::And(left, right) | Node::Or(left, right) => {
            visit(left, found);
            visit(right, found);
        }
        Node::Call(name, arguments) => {
            note_function(name, found);
            arguments.iter().for_each(|argument| visit(argument, found));
        }
    }
}

fn note_function(name: &str, found: &mut References) {
    match name {
        FN_SUCCESS | FN_FAILURE | FN_CANCELLED | FN_ALWAYS => found.status_function = true,
        FN_HASH_FILES => found.hash_files = true,
        _ => {}
    }
}
