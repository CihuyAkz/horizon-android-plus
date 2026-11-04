//! Translation helper used by screen labels and accessibility text.

/// `Component.translatable(key, args)` in the current language.
pub fn translate(key: &str, args: &[&str]) -> String {
    let args: Vec<&dyn std::fmt::Display> =
        args.iter().map(|a| a as &dyn std::fmt::Display).collect();
    crate::lang::tr(key, &args)
}
