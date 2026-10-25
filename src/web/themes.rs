//! Colour themes: ten light, ten dark, plus "automatic" (follows the device). Generated from the Python version's
//! table (mymoney/web/themes.py); `/themes.css` is the same stylesheet, byte for byte.

pub struct Theme {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: &'static str,
    pub colors: [&'static str; 10],
    pub series: [&'static str; 8],
}

const SERIES: [&str; 8] = ["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7"];
const NAMES: [&str; 10] = ["bg", "surface", "text", "muted", "border", "primary", "primary-text", "pos", "neg", "warn"];

const fn t(id: &'static str, label: &'static str, kind: &'static str, bg: &'static str, surface: &'static str, text: &'static str, muted: &'static str, border: &'static str, primary: &'static str, primary_text: &'static str, pos: &'static str, neg: &'static str, warn: &'static str, series: [&'static str; 8]) -> Theme {
    Theme { id, label, kind, colors: [bg, surface, text, muted, border, primary, primary_text, pos, neg, warn], series }
}

pub static THEMES: &[Theme] = &[
    t("clair", "Clair", "light", "#f6f7f9", "#ffffff", "#16181d", "#667085", "#e4e7ec", "#3b5bdb", "#ffffff", "#12805c", "#c4320a", "#b54708", ["#3b5bdb", "#12805c", "#d97706", "#7c3aed", "#0e7490", "#c4320a", "#667085", "#be185d"]),
    t("neige", "Neige", "light", "#f8fafc", "#ffffff", "#0f172a", "#475569", "#e2e8f0", "#2563eb", "#ffffff", "#15803d", "#b91c1c", "#a16207", ["#2563eb", "#15803d", "#d97706", "#7c3aed", "#0e7490", "#b91c1c", "#64748b", "#be185d"]),
    t("sable", "Sable", "light", "#f6efe4", "#fffaf2", "#2b2118", "#6b5b4b", "#e6d9c6", "#a8480a", "#ffffff", "#2a7447", "#b3361b", "#92400e", ["#a8480a", "#2a7447", "#b8790a", "#7e57c2", "#0f766e", "#b3361b", "#7c6f64", "#be185d"]),
    t("menthe", "Menthe", "light", "#eef7f2", "#fbfffd", "#10241b", "#4a6355", "#cfe5d9", "#047857", "#ffffff", "#15803d", "#b42318", "#a15c07", ["#047857", "#2563eb", "#d97706", "#7c3aed", "#0e7490", "#b42318", "#5b6b62", "#be185d"]),
    t("lavande", "Lavande", "light", "#f4f1fb", "#fdfcff", "#1e1633", "#5b5278", "#e2dcf2", "#6d28d9", "#ffffff", "#137a38", "#be123c", "#946007", ["#6d28d9", "#137a38", "#d97706", "#db2777", "#0e7490", "#be123c", "#6b6585", "#2563eb"]),
    t("ciel", "Ciel", "light", "#eef6fc", "#fcfeff", "#0c2233", "#475f73", "#d3e4f1", "#0369a1", "#ffffff", "#15803d", "#b91c1c", "#a16207", ["#0369a1", "#15803d", "#d97706", "#7c3aed", "#0f766e", "#b91c1c", "#5b6b7b", "#be185d"]),
    t("rose", "Rose poudré", "light", "#fbf0f3", "#fffafb", "#2a1620", "#6b4a58", "#f0d5de", "#be185d", "#ffffff", "#15803d", "#b91c1c", "#96610a", ["#be185d", "#15803d", "#d97706", "#7c3aed", "#0e7490", "#b91c1c", "#7a6068", "#2563eb"]),
    t("papier", "Papier", "light", "#f4ecd8", "#fbf5e6", "#3b2f1e", "#6f5f48", "#e2d5b8", "#7c4a1e", "#ffffff", "#3f6b2b", "#a63a1c", "#8a5a00", ["#7c4a1e", "#3f6b2b", "#b7791f", "#6b4c9a", "#2f6f73", "#a63a1c", "#7a6e5a", "#a23b72"]),
    t("solarized-clair", "Solarized clair", "light", "#fdf6e3", "#fefaec", "#3f535a", "#5f7479", "#eee8d5", "#1a6fa8", "#ffffff", "#5f7a00", "#c0261f", "#8a6400", ["#1a6fa8", "#5f7a00", "#8a6400", "#6c71c4", "#1f8f87", "#c0261f", "#7b8d90", "#d33682"]),
    t("agrume", "Agrume", "light", "#fff8e6", "#fffdf6", "#2d2306", "#6a5a2b", "#f0e1b3", "#c2410c", "#ffffff", "#3f7d20", "#b91c1c", "#92400e", ["#c2410c", "#3f7d20", "#b87908", "#7c3aed", "#0e7490", "#b91c1c", "#7a6a3a", "#be185d"]),
    t("sombre", "Sombre", "dark", "#0f1115", "#181b21", "#e6e8ec", "#98a2b3", "#2a2f38", "#6d8bff", "#0f1115", "#3ccf91", "#ff7a59", "#fdb022", ["#6d8bff", "#3ccf91", "#fbbf24", "#a78bfa", "#22d3ee", "#ff7a59", "#98a2b3", "#f472b6"]),
    t("minuit", "Minuit", "dark", "#0a0f1f", "#121a30", "#e4eaff", "#94a3c8", "#223055", "#7aa2ff", "#0a0f1f", "#4ade9b", "#ff7b8a", "#ffc857", ["#7aa2ff", "#4ade9b", "#ffc857", "#b794f6", "#4fd1e8", "#ff7b8a", "#94a3c8", "#f78fc7"]),
    t("ardoise", "Ardoise", "dark", "#0f172a", "#1e293b", "#e2e8f0", "#94a3b8", "#334155", "#38bdf8", "#0f172a", "#34d399", "#fb7185", "#fbbf24", ["#38bdf8", "#34d399", "#fbbf24", "#a78bfa", "#2dd4bf", "#fb7185", "#94a3b8", "#f472b6"]),
    t("charbon", "Charbon", "dark", "#121212", "#1c1c1c", "#ececec", "#a0a0a0", "#333333", "#f5a524", "#121212", "#6bd68c", "#ff6b5e", "#ffcc4d", ["#f5a524", "#6bd68c", "#4ea8ff", "#c792ea", "#4dd0c8", "#ff6b5e", "#a0a0a0", "#ff80ab"]),
    t("foret", "Forêt", "dark", "#0c1712", "#14231b", "#e3f1e8", "#93b3a0", "#25392d", "#4ade80", "#0c1712", "#4ade80", "#fb7c6a", "#facc15", ["#4ade80", "#38bdf8", "#facc15", "#c4b5fd", "#2dd4bf", "#fb7c6a", "#93b3a0", "#f9a8d4"]),
    t("aubergine", "Aubergine", "dark", "#150d1d", "#1f1430", "#efe6fa", "#b3a0cc", "#35254d", "#c084fc", "#150d1d", "#4ade80", "#ff7a90", "#fbbf24", ["#c084fc", "#4ade80", "#fbbf24", "#f0abfc", "#22d3ee", "#ff7a90", "#b3a0cc", "#f472b6"]),
    t("nord", "Nord", "dark", "#2e3440", "#3b4252", "#eceff4", "#aab3c5", "#4c566a", "#88c0d0", "#2e3440", "#a3be8c", "#f2979f", "#ebcb8b", ["#88c0d0", "#a3be8c", "#ebcb8b", "#b48ead", "#8fbcbb", "#f2979f", "#aab3c5", "#d08770"]),
    t("dracula", "Dracula", "dark", "#282a36", "#343746", "#f8f8f2", "#a3acd1", "#44475a", "#bd93f9", "#282a36", "#50fa7b", "#ff8080", "#f1fa8c", ["#bd93f9", "#50fa7b", "#f1fa8c", "#ff79c6", "#8be9fd", "#ff8080", "#a3acd1", "#ffb86c"]),
    t("solarized-sombre", "Solarized sombre", "dark", "#002b36", "#073642", "#eee8d5", "#93a1a1", "#0f4a58", "#35b5ab", "#002b36", "#9bb300", "#f77976", "#d6a300", ["#35b5ab", "#9bb300", "#d6a300", "#8b91d6", "#4aa8e0", "#f77976", "#93a1a1", "#e0569f"]),
    t("oled", "Noir OLED", "dark", "#000000", "#0b0b0c", "#f2f2f2", "#9a9a9f", "#232326", "#4c8dff", "#000000", "#34d27b", "#ff6459", "#ffb020", ["#4c8dff", "#34d27b", "#ffb020", "#a78bfa", "#22d3ee", "#ff6459", "#9a9a9f", "#f472b6"]),
];

impl Theme {
    pub fn css(&self) -> String {
        let mut parts: Vec<String> = NAMES.iter().zip(self.colors.iter()).map(|(k, c)| format!("--{k}: {c}")).collect();
        parts.extend(SERIES.iter().zip(self.series.iter()).map(|(k, c)| format!("--{k}: {c}")));
        let shadow = if self.kind == "light" { "0 1px 2px rgba(16, 24, 40, .06)" } else { "none" };
        let cats = "--cat-liquidites: var(--s0); --cat-epargne: var(--s1); --cat-investissements: var(--s3); --cat-crypto: var(--s2); --cat-immobilier: var(--s4); --cat-autres: var(--s6); --cat-dettes: var(--s5)";
        format!("[data-theme=\"{}\"] {{ {}; {cats}; --shadow: {shadow}; color-scheme: {}; }}", self.id, parts.join("; "), self.kind)
    }
}

pub fn stylesheet() -> String {
    format!("/* Generated from mymoney/web/themes.py */\n{}\n", THEMES.iter().map(|t| t.css()).collect::<Vec<_>>().join("\n"))
}

/// ("Clairs", light themes), ("Sombres", dark themes).
pub fn groups() -> Vec<(&'static str, Vec<&'static Theme>)> {
    vec![("Clairs", THEMES.iter().filter(|t| t.kind == "light").collect()), ("Sombres", THEMES.iter().filter(|t| t.kind == "dark").collect())]
}
