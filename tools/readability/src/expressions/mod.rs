mod fixed_option;

#[cfg(test)]
mod tests;

use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Expr, ExprCall, File};

pub(crate) struct Issue {
    pub(crate) span: Span,
    pub(crate) rule: &'static str,
    pub(crate) message: &'static str,
}

#[derive(Default)]
pub(crate) struct Options {
    pub(crate) fixed_option_returns: bool,
}

struct Expressions<'a> {
    issues: Vec<Issue>,
    options: &'a Options,
}

impl<'ast> Visit<'ast> for Expressions<'_> {
    fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
        if self.options.fixed_option_returns {
            self.issues
                .extend(fixed_option::inspect(&function.sig, &function.block));
        }

        visit::visit_item_fn(self, function);
    }

    fn visit_impl_item_fn(&mut self, function: &'ast syn::ImplItemFn) {
        if self.options.fixed_option_returns {
            self.issues
                .extend(fixed_option::inspect(&function.sig, &function.block));
        }

        visit::visit_impl_item_fn(self, function);
    }

    fn visit_trait_item_fn(&mut self, function: &'ast syn::TraitItemFn) {
        if self.options.fixed_option_returns
            && let Some(body) = &function.default
        {
            self.issues
                .extend(fixed_option::inspect(&function.sig, body));
        }

        visit::visit_trait_item_fn(self, function);
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        let mut callee = call.func.as_ref();

        loop {
            match callee {
                Expr::Paren(expr) => callee = &expr.expr,
                Expr::Group(expr) => callee = &expr.expr,
                _ => break,
            }
        }

        if matches!(callee, Expr::Closure(_)) {
            self.issues.push(Issue {
                span: call.span(),
                rule: "immediate-closure-call",
                message: "extract a named function instead of immediately calling an anonymous closure",
            });
        }

        visit::visit_expr_call(self, call);
    }
}

pub(crate) fn inspect(file: &File, options: &Options) -> Vec<Issue> {
    let mut expressions = Expressions {
        issues: Vec::new(),
        options,
    };

    expressions.visit_file(file);

    expressions.issues
}
