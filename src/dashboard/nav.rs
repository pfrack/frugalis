/// Navigation page entry registered in `PAGES`.
///
/// # Safety
/// The `icon` field must contain a trusted SVG string (compile-time constant).
/// It is rendered with `|safe` in `base.html` — bypassing HTML escaping.
/// Never source `icon` from user input, a database, or any untrusted origin.
pub struct NavPage {
    pub path: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
}

pub struct NavItem {
    pub path: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
    pub active: bool,
}

pub struct NavContext {
    pub pages: Vec<NavItem>,
}

const ICON_DASHBOARD: &str = "<svg width='16' height='16' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><rect x='3' y='3' width='7' height='7'/><rect x='14' y='3' width='7' height='7'/><rect x='3' y='14' width='7' height='7'/><rect x='14' y='14' width='7' height='7'/></svg>";
const ICON_LIST: &str = "<svg width='16' height='16' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><line x1='8' y1='6' x2='21' y2='6'/><line x1='8' y1='12' x2='21' y2='12'/><line x1='8' y1='18' x2='21' y2='18'/><line x1='3' y1='6' x2='3.01' y2='6'/><line x1='3' y1='12' x2='3.01' y2='12'/><line x1='3' y1='18' x2='3.01' y2='18'/></svg>";
const ICON_CLOCK: &str = "<svg width='16' height='16' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><circle cx='12' cy='12' r='10'/><polyline points='12 6 12 12 16 14'/></svg>";
const ICON_DOLLAR: &str = "<svg width='16' height='16' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><line x1='12' y1='1' x2='12' y2='23'/><path d='M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6'/></svg>";
const ICON_CACHE: &str = "<svg width='16' height='16' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M21 12a9 9 0 0 0-8.17-8.98'/><path d='M3 12a9 9 0 0 0 8.17 8.98'/><polyline points='15 7 21 7 21 1'/><polyline points='9 17 3 17 3 23'/></svg>";

pub static PAGES: &[NavPage] = &[
    NavPage {
        path: "",
        label: "Dashboard",
        icon: ICON_DASHBOARD,
    },
    NavPage {
        path: "inferences",
        label: "Inference Logs",
        icon: ICON_LIST,
    },
    NavPage {
        path: "latency",
        label: "Latency",
        icon: ICON_CLOCK,
    },
    NavPage {
        path: "savings",
        label: "Savings",
        icon: ICON_DOLLAR,
    },
    NavPage {
        path: "cache",
        label: "Cache",
        icon: ICON_CACHE,
    },
];

pub fn nav_for(current: &str) -> NavContext {
    NavContext {
        pages: PAGES
            .iter()
            .map(|p| NavItem {
                path: p.path,
                label: p.label,
                icon: p.icon,
                active: p.path == current,
            })
            .collect(),
    }
}
