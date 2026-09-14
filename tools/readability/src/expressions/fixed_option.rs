use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Block, Expr, Path, ReturnType, Signature, Stmt, Type};

use crate::expressions::Issue;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    Some,
    None,
}

struct Returns {
    expected: Variant,
    uncertain: bool,
    try_depth: usize,
}

impl<'ast> Visit<'ast> for Returns {
    fn visit_expr_return(&mut self, expression: &'ast syn::ExprReturn) {
        if expression.expr.as_deref().and_then(direct_variant) != Some(self.expected) {
            self.uncertain = true;
        }

        visit::visit_expr_return(self, expression);
    }

    fn visit_expr_try(&mut self, expression: &'ast syn::ExprTry) {
        if self.try_depth == 0 && self.expected != Variant::None {
            self.uncertain = true;
        }

        visit::visit_expr_try(self, expression);
    }

    fn visit_macro(&mut self, _: &'ast syn::Macro) {
        // Macro expansion can introduce an early return with another variant.
        self.uncertain = true;
    }

    fn visit_expr_try_block(&mut self, expression: &'ast syn::ExprTryBlock) {
        // A try block captures `?`, but an explicit return still exits the
        // enclosing function and must participate in the variant check.
        self.try_depth += 1;

        visit::visit_expr_try_block(self, expression);

        self.try_depth -= 1;
    }

    // These bodies have their own return or propagation scope. Their return
    // values cannot establish how the enclosing function returns.
    fn visit_item(&mut self, _: &'ast syn::Item) {}

    fn visit_expr_closure(&mut self, _: &'ast syn::ExprClosure) {}

    fn visit_expr_async(&mut self, _: &'ast syn::ExprAsync) {}
}

fn has_path(path: &Path, expected: &[&str]) -> bool {
    path.segments.len() == expected.len()
        && path
            .segments
            .iter()
            .zip(expected)
            .all(|(segment, name)| segment.ident == *name)
}

fn returns_option(output: &ReturnType) -> bool {
    let ReturnType::Type(_, output) = output else {
        return false;
    };

    let mut output = output.as_ref();

    loop {
        match output {
            Type::Paren(ty) => output = &ty.elem,
            Type::Group(ty) => output = &ty.elem,
            _ => break,
        }
    }

    let Type::Path(output) = output else {
        return false;
    };

    output.qself.is_none()
        && (has_path(&output.path, &["Option"])
            || has_path(&output.path, &["std", "option", "Option"])
            || has_path(&output.path, &["core", "option", "Option"]))
}

fn is_variant(path: &Path, name: &str) -> bool {
    has_path(path, &[name])
        || has_path(path, &["Option", name])
        || has_path(path, &["std", "option", "Option", name])
        || has_path(path, &["core", "option", "Option", name])
}

fn direct_variant(expression: &Expr) -> Option<Variant> {
    match expression {
        Expr::Paren(expr) => direct_variant(&expr.expr),
        Expr::Group(expr) => direct_variant(&expr.expr),
        Expr::Return(expr) => expr.expr.as_deref().and_then(direct_variant),
        Expr::Path(expr) if expr.qself.is_none() && is_variant(&expr.path, "None") => {
            Some(Variant::None)
        }
        Expr::Call(expr) if expr.args.len() == 1 => {
            let Expr::Path(callee) = expr.func.as_ref() else {
                return None;
            };

            (callee.qself.is_none() && is_variant(&callee.path, "Some")).then_some(Variant::Some)
        }
        _ => None,
    }
}

pub(super) fn inspect(signature: &Signature, body: &Block) -> Option<Issue> {
    if !returns_option(&signature.output) {
        return None;
    }

    let Stmt::Expr(tail, _) = body.stmts.last()? else {
        return None;
    };

    let expected = direct_variant(tail)?;
    let mut returns = Returns {
        expected,
        uncertain: false,
        try_depth: 0,
    };

    returns.visit_block(body);

    if returns.uncertain {
        return None;
    }

    let message = match expected {
        Variant::Some => "this function only returns Some; review its Option return type",
        Variant::None => "this function only returns None; review its return value and callers",
    };

    Some(Issue {
        span: signature.span().join(body.span()).unwrap_or(body.span()),
        rule: "fixed-option-return",
        message,
    })
}
