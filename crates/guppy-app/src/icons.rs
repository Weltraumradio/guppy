//! Stroke icons for the menu bar (24×24, tinted at runtime).

macro_rules! icon {
    ($name:ident, $body:literal) => {
        pub const $name: &str = concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">"#,
            $body,
            "</svg>"
        );
    };
}

icon!(BACK, r#"<path d="M15 18l-6-6 6-6"/>"#);
icon!(FORWARD, r#"<path d="M9 6l6 6-6 6"/>"#);
icon!(UP, r#"<path d="M12 19V5M5 12l7-7 7 7"/>"#);
icon!(HOME, r#"<path d="M3 11l9-8 9 8M5 10v10h14V10"/>"#);
icon!(NEW_DIR, r#"<path d="M3 7h6l2 2h10v10H3zM12 11.5v5M9.5 14h5"/>"#);
icon!(COPY, r#"<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V5a1 1 0 0 0-1-1H5a1 1 0 0 0-1 1v10a1 1 0 0 0 1 1h3"/>"#);
icon!(MOVE, r#"<path d="M4 12h15M13 6l6 6-6 6"/>"#);
icon!(DELETE, r#"<path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13"/>"#);
icon!(MARK, r#"<path d="M5 12l4 4L19 6"/>"#);
icon!(SWAP, r#"<path d="M7 7h13M16 3l4 4-4 4M17 17H4M8 13l-4 4 4 4"/>"#);
icon!(HIDDEN, r#"<path d="M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12z"/><circle cx="12" cy="12" r="3"/>"#);
icon!(SEARCH, r#"<circle cx="11" cy="11" r="7"/><path d="M20 20l-3.5-3.5"/>"#);
icon!(SETTINGS, r#"<path d="M4 7h10M18 7h2M4 17h4M12 17h8"/><circle cx="16" cy="7" r="2"/><circle cx="10" cy="17" r="2"/>"#);
