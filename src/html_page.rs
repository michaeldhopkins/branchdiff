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
function showTheme(theme){
  document.documentElement.dataset.theme=theme;
  const btn=document.querySelector('.theme-toggle');
  if(btn)btn.textContent=theme==='light'?'☾':'☀';
}
function applyTheme(){
  let theme=document.documentElement.dataset.theme||'dark';
  try{theme=localStorage.getItem('bd-theme')||theme}catch(e){}
  showTheme(theme);
}
function bindToggle(){
  document.querySelector('.theme-toggle')?.addEventListener('click',()=>{
    const next=document.documentElement.dataset.theme==='light'?'dark':'light';
    try{localStorage.setItem('bd-theme',next)}catch(e){}
    showTheme(next);
  });
}
applyTheme();
bindToggle();
";

/// Remembers the body as served, before the theme script touches the toggle,
/// so the first poll can tell that nothing changed.
const LIVE_START_JS: &str = "let prev=document.body.innerHTML.trimEnd();\n";

/// Each file section sits in `content-visibility: auto`, which remembers the height a
/// section had once rendered, per element. Swapping the body in makes every element
/// new, so each new section is given its old section's measured height (matched by file
/// path, which survives files being added or removed) as its intrinsic size. A reader who
/// has scrolled is then anchored on the first section still on screen and scrolled back
/// to the offset it had; one at the top, or whose first section lies below the window,
/// keeps the plain scroll position. A section the page has not seen before renders for
/// the first time once it nears the window and changes height, so the anchor is put back
/// each frame until it holds still.
const LIVE_JS: &str = "\
if(location.protocol!=='file:'){let last='';setInterval(()=>{
  fetch(location.href,{cache:'no-store'}).then(r=>r.text()).then(h=>{
    if(h===last)return;
    last=h;
    const d=new DOMParser().parseFromString(h,'text/html');
    const newBody=d.body.innerHTML;
    const key=newBody.trimEnd();
    if(key===prev||!newBody)return;
    prev=key;
    const sections=()=>[...document.querySelectorAll('details[data-path]')];
    const contentHeight=e=>{
      const box=getComputedStyle(e),rect=e.getBoundingClientRect();
      return rect.height-['borderTopWidth','borderBottomWidth','paddingTop','paddingBottom']
        .reduce((sum,side)=>sum+parseFloat(box[side]),0);
    };
    const old=sections();
    const s=[window.scrollX,window.scrollY];
    const first=s[1]>0&&old.find(e=>e.getBoundingClientRect().bottom>0);
    const keep=first&&first.getBoundingClientRect().top<window.innerHeight&&{path:first.dataset.path,top:first.getBoundingClientRect().top};
    const heights=new Map(old.map(e=>[e.dataset.path,contentHeight(e)]));
    document.body.innerHTML=newBody;
    const fresh=sections();
    for(const e of fresh){
      const height=heights.get(e.dataset.path);
      if(height)e.style.containIntrinsicSize='auto '+height+'px';
    }
    const now=keep&&fresh.find(e=>e.dataset.path===keep.path);
    if(now){
      now.scrollIntoView({block:'start',inline:'nearest'});
      window.scrollTo(s[0],window.scrollY-keep.top);
      let held=0,frames=0;
      const hold=()=>{
        const off=now.getBoundingClientRect().top-keep.top;
        if(Math.abs(off)>0.5){window.scrollBy(0,off);held=0}else held++;
        if(held<2&&++frames<12)requestAnimationFrame(hold);
      };
      requestAnimationFrame(hold);
    }
    else window.scrollTo(...s);
    applyTheme();
    bindToggle();
  }).catch(()=>{});
},2000)}
";

/// Sets the stored theme from the head, so the first paint is already in it. The
/// body script still syncs the toggle's glyph once the button exists.
pub(crate) const HEAD_THEME_SCRIPT: &str = "<script>try{document.documentElement.dataset.theme=localStorage.getItem('bd-theme')||'dark'}catch(e){}</script>";

/// The page's `<script>` element.
pub fn script(refresh: Refresh) -> String {
    cached_script(refresh).to_owned()
}

/// The same text, built once per process for each mode.
pub(crate) fn cached_script(refresh: Refresh) -> &'static str {
    static LIVE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    static OFF: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    match refresh {
        Refresh::Live => LIVE.get_or_init(|| build_script(Refresh::Live)),
        Refresh::Off => OFF.get_or_init(|| build_script(Refresh::Off)),
    }
}

fn build_script(refresh: Refresh) -> String {
    let (start, live) = match refresh {
        Refresh::Live => (LIVE_START_JS, LIVE_JS),
        Refresh::Off => ("", ""),
    };
    const OPEN: &str = "<script>\n";
    const CLOSE: &str = "</script>";
    let mut page =
        String::with_capacity(OPEN.len() + start.len() + THEME_JS.len() + live.len() + CLOSE.len());
    page.push_str(OPEN);
    page.push_str(start);
    page.push_str(THEME_JS);
    page.push_str(live);
    page.push_str(CLOSE);
    page
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
    fn the_served_body_is_remembered_before_the_theme_script_changes_it() {
        let s = script(Refresh::Live);
        let remembered =
            s.find("let prev=document.body.innerHTML.trimEnd();").expect("prev is initialised");
        assert!(remembered < s.find("applyTheme();").unwrap());
        assert!(!script(Refresh::Off).contains("prev"));
    }

    #[test]
    fn an_unchanged_response_is_not_parsed_again() {
        let s = script(Refresh::Live);
        let skip = s.find("if(h===last)return;").expect("compares the response text");
        assert!(skip < s.find("DOMParser").unwrap());
    }

    #[test]
    fn the_reader_is_anchored_by_path_and_only_when_scrolled_into_the_page() {
        let s = script(Refresh::Live);
        assert!(s.contains("e.dataset.path"), "sections are matched by path, not position");
        assert!(!s.contains("keep.id") && !s.contains("heights.get(e.id)"), "{s}");
        assert!(s.contains("s[1]>0"), "a reader at the top is never anchored");
        assert!(s.contains("innerHeight"), "an anchor below the window is not a place");
    }

    #[test]
    fn the_anchor_branch_restores_the_horizontal_scroll() {
        let s = script(Refresh::Live);
        assert!(s.contains("scrollTo(s[0],window.scrollY-keep.top)"), "{s}");
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
