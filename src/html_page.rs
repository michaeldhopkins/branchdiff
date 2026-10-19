//! The scripts and script-less fallbacks of the `--html` page.

use crate::html_style::LIGHT_PALETTE;

/// Whether the page re-fetches itself while served over HTTP.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Refresh {
    /// Poll the page's own address every two seconds and swap in a changed body.
    Live,
    /// No polling: the page is a snapshot.
    #[value(name = "static")]
    Off,
}

const THEME_JS: &str = "\
function applyTheme(){
  const theme=localStorage.getItem('bd-theme')||'dark';
  document.documentElement.dataset.theme=theme;
  const btn=document.querySelector('.theme-toggle');
  if(btn)btn.textContent=theme==='light'?'☾':'☀';
}
function bindToggle(){
  document.querySelector('.theme-toggle')?.addEventListener('click',()=>{
    const next=document.documentElement.dataset.theme==='light'?'dark':'light';
    localStorage.setItem('bd-theme',next);
    applyTheme();
  });
}
applyTheme();
bindToggle();
";

const LIVE_JS: &str = "\
if(location.protocol!=='file:'){let prev='';setInterval(()=>{
  fetch(location.href,{cache:'no-store'}).then(r=>r.text()).then(h=>{
    const d=new DOMParser().parseFromString(h,'text/html');
    const newBody=d.body.innerHTML;
    if(newBody===prev||!newBody)return;
    prev=newBody;
    const s=[window.scrollX,window.scrollY];
    document.body.innerHTML=newBody;
    window.scrollTo(...s);
    applyTheme();
    bindToggle();
  }).catch(()=>{});
},2000)}
";

/// The page's `<script>` element.
pub fn script(refresh: Refresh) -> String {
    let live = match refresh {
        Refresh::Live => LIVE_JS,
        Refresh::Off => "",
    };
    format!("<script>\n{THEME_JS}{live}</script>")
}

/// For a reader with scripts off (a sandboxed iframe, a mail client): follow
/// the system colour scheme, and hide the toggle that cannot work.
pub fn noscript_style() -> String {
    format!(
        "<noscript><style>\n\
         .theme-toggle {{ display: none; }}\n\
         @media (prefers-color-scheme: light) {{\n\
         :root {{\n{LIGHT_PALETTE}\n}}\n\
         }}\n\
         </style></noscript>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_page_polls_itself() {
        let s = script(Refresh::Live);
        assert!(s.contains("setInterval"));
        assert!(s.contains("fetch(location.href"));
    }

    #[test]
    fn a_page_with_refresh_off_never_fetches_or_sets_a_timer() {
        let s = script(Refresh::Off);
        for forbidden in
            ["setInterval", "setTimeout", "fetch(", "XMLHttpRequest", "EventSource", "WebSocket"]
        {
            assert!(!s.contains(forbidden), "{forbidden} in a page with refresh off");
        }
    }

    #[test]
    fn the_theme_toggle_works_in_both_modes() {
        for refresh in [Refresh::Live, Refresh::Off] {
            let s = script(refresh);
            assert!(s.contains("bindToggle();"), "{refresh:?}");
            assert!(s.contains("localStorage.setItem('bd-theme'"), "{refresh:?}");
            assert!(s.starts_with("<script>") && s.ends_with("</script>"));
        }
    }

    #[test]
    fn without_scripts_the_toggle_is_hidden_and_light_follows_the_system() {
        let css = noscript_style();
        assert!(css.starts_with("<noscript><style>") && css.ends_with("</style></noscript>"));
        assert!(css.contains(".theme-toggle { display: none; }"));
        assert!(css.contains("@media (prefers-color-scheme: light)"));
        assert!(css.contains(LIGHT_PALETTE));
    }
}
