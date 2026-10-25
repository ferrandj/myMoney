//! Colour themes: thirty light, thirty dark, plus "automatic" (follows the device). Generated from the Python version's
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
    t("aube", "Aube", "light", "#f6f1ee", "#fefcfb", "#211712", "#756157", "#e6dbd6", "#af4e1d", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#b15425", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#816e65", "#b1256b"]),
    t("brume", "Brume", "light", "#eff3f7", "#fbfcfe", "#121821", "#576375", "#d6dce6", "#1e5eb8", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#255fb1", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#657081", "#b1256b"]),
    t("coton", "Coton", "light", "#f8f0f3", "#fefbfc", "#211217", "#755761", "#e6d6db", "#b81e51", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#b12554", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#81656e", "#b1256b"]),
    t("perle", "Perle", "light", "#f4f1f9", "#fcfbfe", "#171221", "#615775", "#dbd6e6", "#511eb8", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#5425b1", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#6e6581", "#b1256b"]),
    t("lin", "Lin", "light", "#f8f4ec", "#fefdfb", "#211c12", "#756b57", "#e6e1d6", "#1e1eb8", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#2525b1", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#817765", "#b1256b"]),
    t("amande", "Amande", "light", "#f7f4f0", "#fefdfb", "#211a12", "#756757", "#e6ded6", "#1e33b8", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#2538b1", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#817465", "#b1256b"]),
    t("pistache", "Pistache", "light", "#f4f8f1", "#fcfefb", "#182112", "#637557", "#dce6d6", "#3f7b14", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#56a022", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#708165", "#b1256b"]),
    t("citron", "Citron", "light", "#f9f8f2", "#fefdfb", "#212012", "#727055", "#e6e5d6", "#451eb8", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#4825b1", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#817e65", "#b1256b"]),
    t("abricot", "Abricot", "light", "#f8f2ed", "#fefcfb", "#211912", "#756557", "#e6ddd6", "#1e3db8", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#2541b1", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#817265", "#b1256b"]),
    t("corail", "Corail", "light", "#f9efee", "#fefcfb", "#211412", "#755b57", "#e6d8d6", "#b8331e", "#ffffff", "#117845", "#b62c16", "#8e570b", ["#b13825", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#816965", "#b1256b"]),
    t("peche", "Pêche", "light", "#f8f3f1", "#fefcfb", "#211612", "#755e57", "#e6dad6", "#b8451e", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#b14825", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#816c65", "#b1256b"]),
    t("framboise", "Framboise", "light", "#f9f2f5", "#fefbfc", "#211218", "#755763", "#e6d6dc", "#b81e5e", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#b1255f", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#816570", "#b1256b"]),
    t("lilas", "Lilas", "light", "#f4edf7", "#fdfbfe", "#1c1221", "#6b5775", "#e1d6e6", "#851eb8", "#ffffff", "#117845", "#b62c16", "#8e570b", ["#8225b1", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#776581", "#b1256b"]),
    t("glycine", "Glycine", "light", "#f0eef8", "#fcfbfe", "#141221", "#5c5775", "#d8d6e6", "#381eb8", "#ffffff", "#117845", "#b62c16", "#8e570b", ["#3d25b1", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#6a6581", "#b1256b"]),
    t("ocean", "Océan", "light", "#eff6f9", "#fbfdfe", "#121c21", "#576b75", "#d6e1e6", "#1a75a2", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#2582b1", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#657781", "#b1256b"]),
    t("lagune", "Lagune", "light", "#f2f8f8", "#fbfefd", "#122120", "#577573", "#d6e6e5", "#147b72", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#22a095", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#65817e", "#b1256b"]),
    t("givre", "Givre", "light", "#eef5f7", "#fbfdfe", "#121f21", "#577075", "#d6e3e6", "#167588", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#259ab1", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#657c81", "#b1256b"]),
    t("argile", "Argile", "light", "#f8f1ef", "#fefcfb", "#211612", "#756057", "#e6dbd6", "#1e57b8", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#2559b1", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#816d65", "#b1256b"]),
    t("nuage", "Nuage", "light", "#f0f2f9", "#fbfcfe", "#121621", "#575e75", "#d6dae6", "#1e45b8", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#2548b1", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#656c81", "#b1256b"]),
    t("pivoine", "Pivoine", "light", "#faf1f6", "#fefbfd", "#21121b", "#755769", "#e6d6df", "#b81e78", "#ffffff", "#127d47", "#b62c16", "#8e570b", ["#b12577", "#23a463", "#b18225", "#6b25b1", "#259ab1", "#b13825", "#816575", "#b1256b"]),
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
    t("cosmos", "Cosmos", "dark", "#0d0c16", "#181528", "#e7e6f0", "#928db0", "#2f2a46", "#8771f4", "#0d0c16", "#54d494", "#f77064", "#fac038", ["#8771f4", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#9692aa", "#ec519e"]),
    t("orage", "Orage", "dark", "#0c0f18", "#161b2a", "#e6e8f0", "#8d95b0", "#2a3146", "#5f83f2", "#0c0f18", "#54d494", "#f77064", "#fac038", ["#5f83f2", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#9298aa", "#ec519e"]),
    t("encre", "Encre", "dark", "#0c0f1b", "#181b2c", "#e6e7f0", "#8d92b0", "#2a2f46", "#687ff3", "#0c0f1b", "#54d494", "#f77064", "#fac038", ["#687ff3", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#9296aa", "#ec519e"]),
    t("obsidienne", "Obsidienne", "dark", "#150c1e", "#23192e", "#ebe6f0", "#9e8db0", "#382a46", "#5fc1f2", "#150c1e", "#54d494", "#f77064", "#fac038", ["#5fc1f2", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#9e92aa", "#ec519e"]),
    t("sapin", "Sapin", "dark", "#091811", "#15281f", "#e6f0eb", "#8db09e", "#2a4638", "#5ff2a8", "#091811", "#54d494", "#f77064", "#fac038", ["#5ff2a8", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#92aa9e", "#ec519e"]),
    t("bordeaux", "Bordeaux", "dark", "#180d0e", "#2a161a", "#f0e6e7", "#b08d92", "#462a2f", "#f25f77", "#180d0e", "#54d494", "#f77064", "#fac038", ["#f25f77", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#aa9296", "#ec519e"]),
    t("prune", "Prune", "dark", "#1a0d1a", "#2c182c", "#f0e6f0", "#b08db0", "#462a46", "#f25ff2", "#1a0d1a", "#54d494", "#f77064", "#fac038", ["#f25ff2", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#aa92aa", "#ec519e"]),
    t("cobalt", "Cobalt", "dark", "#0d141d", "#19212e", "#e6eaf0", "#8d9bb0", "#2a3646", "#5f9cf2", "#0d141d", "#54d494", "#f77064", "#fac038", ["#5f9cf2", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#929caa", "#ec519e"]),
    t("abysse", "Abysse", "dark", "#0a1518", "#152528", "#e6eef0", "#8daab0", "#2a4146", "#5fd9f2", "#0a1518", "#54d494", "#f77064", "#fac038", ["#5fd9f2", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#92a6aa", "#ec519e"]),
    t("braise", "Braise", "dark", "#1a0d0a", "#2a1a16", "#f0e8e6", "#b0948d", "#46302a", "#f27c5f", "#1a0d0a", "#54d494", "#f77064", "#fac038", ["#f27c5f", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#aa9792", "#ec519e"]),
    t("cuivre", "Cuivre", "dark", "#1a130e", "#2c2018", "#f0eae6", "#b09b8d", "#46362a", "#f29c5f", "#1a130e", "#54d494", "#f77064", "#fac038", ["#f29c5f", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#aa9c92", "#ec519e"]),
    t("mousse", "Mousse", "dark", "#101c0e", "#1c2e19", "#e7f0e6", "#92b08d", "#2f462a", "#77f25f", "#101c0e", "#54d494", "#f77064", "#fac038", ["#77f25f", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#96aa92", "#ec519e"]),
    t("crepuscule", "Crépuscule", "dark", "#170a13", "#281522", "#f0e6ec", "#b08da4", "#462a3d", "#f25fc1", "#170a13", "#54d494", "#f77064", "#fac038", ["#f25fc1", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#aa92a2", "#ec519e"]),
    t("aurore", "Aurore", "dark", "#0b1a16", "#162a25", "#e6f0ed", "#8db0a7", "#2a463f", "#5ff2cd", "#0b1a16", "#54d494", "#f77064", "#fac038", ["#5ff2cd", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#92aaa4", "#ec519e"]),
    t("fusain", "Fusain", "dark", "#0b141d", "#18222c", "#e6ebf0", "#8d9eb0", "#2a3846", "#5fc1f2", "#0b141d", "#54d494", "#f77064", "#fac038", ["#5fc1f2", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#929eaa", "#ec519e"]),
    t("ebene", "Ébène", "dark", "#1c150f", "#2e2319", "#f0ebe6", "#b09e8d", "#46382a", "#5fc1f2", "#1c150f", "#54d494", "#f77064", "#fac038", ["#5fc1f2", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#aa9e92", "#ec519e"]),
    t("petrole", "Pétrole", "dark", "#0b1616", "#152828", "#e6f0f0", "#8db0b0", "#2a4646", "#5ff2f2", "#0b1616", "#54d494", "#f77064", "#fac038", ["#5ff2f2", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#92aaaa", "#ec519e"]),
    t("neon", "Néon", "dark", "#170b19", "#27162a", "#eee6f0", "#aa8db0", "#412a46", "#d95ff2", "#170b19", "#54d494", "#f77064", "#fac038", ["#d95ff2", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#a692aa", "#ec519e"]),
    t("cerise", "Cerise", "dark", "#1c0b10", "#2c181d", "#f0e6e8", "#b08d95", "#462a31", "#f25f83", "#1c0b10", "#54d494", "#f77064", "#fac038", ["#f25f83", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#aa9298", "#ec519e"]),
    t("brouillard", "Brouillard", "dark", "#0b0b1f", "#19192e", "#e6e6f0", "#8d8db0", "#2a2a46", "#7b7bf4", "#0b0b1f", "#54d494", "#f77064", "#fac038", ["#7b7bf4", "#51ec9e", "#ecc551", "#9e51ec", "#51dfec", "#ec5e51", "#9292aa", "#ec519e"]),
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
