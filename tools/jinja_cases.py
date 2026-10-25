"""Template snippets rendered by Jinja2 (as Starlette sets it up) and by the Rust engine, to compare byte for byte.

Values are tagged in JSON: {"$dec": "1.5"}, {"$date": "2024-01-31"}, {"$dt": "2024-01-31T10:00:00"}, {"$markup": "<b>"},
{"$tuple": [...]}, {"$obj": {...}} (attributes only), {"$float": 1.0}.
"""
import json, sys

CASES = [
    ("arith", "{{ 1 + 2 }} {{ 7 / 2 }} {{ 7 // 2 }} {{ -7 // 2 }} {{ -7 % 3 }} {{ 2 * 3.5 }} {{ 1024 / 1024 }} {{ 10 / 4 }}", {}),
    ("dec", "{{ a + b }} {{ a - a }} {{ a * 100 }} {{ a / b }} {{ 1 - a / b }} {{ (1 - a / b) * 100 }} {{ -z }} {{ a > 0 }} {{ a == 1.5 }} {{ z == 0 }}",
     {"a": {"$dec": "1.50"}, "b": {"$dec": "3.3"}, "z": {"$dec": "0.00"}}),
    ("dec_round", "{{ ((1 - a / b) * 100)|round(0)|int }} {{ ((1 - a / b) * 100)|round(1) }} {{ x|round(1) }} {{ (s / 1024)|round(0)|int }} {{ 2.5|round }} {{ 0.125|round(2) }}",
     {"a": {"$dec": "123456.78"}, "b": {"$dec": "250000"}, "x": {"$dec": "45.35"}, "s": 153600}),
    ("fmt", "{{ '%.2f'|format(p) }}|{{ '%.1f'|format(p)|replace('.', ',') }}|{{ '%.0f'|format(q) }}|{{ '%.2f'|format(r) }}|{{ '%.1f'|format(n) }}|{{ '%s et %s'|format('a<', 'b') }}",
     {"p": {"$dec": "12.345"}, "q": {"$dec": "0.5"}, "r": {"$float": 2.675}, "n": {"$dec": "-0.04"}}),
    ("print", "{{ none }}|{{ true }}|{{ x }}|{{ f }}|{{ g }}|{{ d }}|{{ dt }}|{{ l }}|{{ t }}|{{ u }}|{{ e }}|{{ big }}|{{ small }}",
     {"x": None, "f": {"$float": 0.1}, "g": {"$float": 100.0}, "d": {"$date": "2024-02-29"}, "dt": {"$dt": "2024-02-29T08:05:00"},
      "l": ["a", 1, None], "t": {"$tuple": [1]}, "e": {"$dec": "1E+2"}, "big": {"$float": 1e16}, "small": {"$dec": "0.0000001"}}),
    ("escape", "{{ s }}|{{ m }}|{{ s|e }}|{{ m|e }}|{{ s|safe }}|{{ (s ~ m) }}|{{ 'a' ~ 1 ~ none }}|{{ s|e|replace('\\n', '<br>'|safe) }}",
     {"s": "<a href=\"x\">'&'</a>\nz", "m": {"$markup": "<b>ok</b>"}}),
    ("join", "{{ [a, b, '', none]|select|join(' · ') }}|{{ [m, 'x<']|join(', ') }}|{{ items|join(', ', attribute='n') }}",
     {"a": "Bourso", "b": "Compte <courant>", "m": {"$markup": "<i>i</i>"}, "items": [{"n": 1}, {"n": 2}]}),
    ("selectattr", "{{ ads|selectattr('id', 'equalto', 'b')|map(attribute='label')|first }}|{{ ads|selectattr('id', 'equalto', 'z')|map(attribute='label')|first or '' }}|{{ plans|selectattr('st.kind', 'in', ['info', 'x'])|list|length }}",
     {"ads": [{"$obj": {"id": "a", "label": "A"}}, {"$obj": {"id": "b", "label": "B<"}}], "plans": [{"$obj": {"st": {"$obj": {"kind": "info"}}}}, {"$obj": {"st": {"$obj": {"kind": "statement"}}}}]}),
    ("sum_min", "{{ fl|sum(attribute='i') }}|{{ []|sum(attribute='i') }}|{{ (fl|sum(attribute='i') - fl|sum(attribute='o')) }}|{{ [3, 8 - 1]|min }}|{{ ['b', 'A']|min }}|{{ fl|length }}|{{ 'été'|length }}",
     {"fl": [{"$obj": {"i": {"$dec": "10.50"}, "o": {"$dec": "3"}}}, {"$obj": {"i": {"$dec": "0.25"}, "o": {"$dec": "1.00"}}}]}),
    ("loops", "{% for x in xs if x > 1 %}{{ loop.index }}/{{ loop.index0 }}/{{ loop.first }}/{{ loop.last }}:{{ x }} {% else %}vide{% endfor %}|{% for x in [] %}a{% else %}vide{% endfor %}|{% for k, (a, b) in pairs %}{{ k }}={{ a }}{{ b }};{% endfor %}|{% for a, b in d.items() %}{{ a }}{{ b }}{% endfor %}",
     {"xs": [1, 2, 3, 0, 5], "pairs": [["k", ["x", "y"]], ["j", {"$tuple": ["1", "2"]}]], "d": {"z": 1, "a": 2}}),
    ("scope", "{% set x = 1 %}{% for i in [1, 2] %}{% set x = x + i %}{{ x }}{% endfor %}|{{ x }}|{% if true %}{% set y = 5 %}{% endif %}{{ y }}", {}),
    ("cond", "[{{ 's' if n > 1 }}][{{ 'a' if n else 'b' }}][{{ v if v is not none else -1 }}][{{ 'x' if u is defined else 'y' }}][{{ not w is none }}]", {"n": 1, "v": None, "w": 3}),
    ("undef", "[{{ u }}][{{ u|length }}][{{ d.missing }}][{{ d.get('k') }}][{{ d.get('k', 'def') }}][{{ n.attr }}][{% for x in u %}x{% endfor %}][{{ u or 'o' }}][{{ d['missing'] }}][{{ l[5] }}]",
     {"d": {"a": 1}, "n": None, "l": [1]}),
    ("dict", "{{ {'high': 'élevée', 'low': 'faible'}[c] }}|{{ D.get(s, s) }}|{{ k in D }}|{{ 'x' in 'axe' }}|{{ 3 not in [1, 2] }}|{{ D.items()|list|length }}|{{ D[1] }}|{{ D[1.0] }}",
     {"c": "low", "s": "zz", "k": "a", "D": {"a": "A"}}),
    ("dictint", "{{ m[1] }}|{{ m.get(2, '-') }}|{{ 1 in m }}", {"m": {"$intdict": [[1, "un"], [3, "trois"]]}}),
    ("slices", "{{ l[:2] }}|{{ l[1:] }}|{{ l[-1] }}|{{ 'abcdef'[2:4] }}|{{ l[::-1] }}", {"l": [1, 2, 3, 4]}),
    ("strings", "{{ 'Copier l\\'IBAN' }}|{{ \"a\\tb\" }}|{{ p.startswith('/doc') }}|{{ p.startswith(('/x', '/d')) }}|{{ 'ÉTÉ'|lower }}|{{ ('a'|e)|lower }}", {"p": "/documents/3"}),
    ("dates", "{{ d.isoformat() }}|{{ d.strftime('%m/%Y') }}|{{ d.strftime('%d/%m/%y') }}|{{ dt.isoformat() }}|{{ dt.strftime('%d/%m/%Y %H:%M') }}|{{ d.year }}|{{ d == d2 }}|{{ d < d2 }}",
     {"d": {"$date": "2024-03-05"}, "d2": {"$date": "2024-03-06"}, "dt": {"$dt": "2024-03-05T07:08:09.000120"}}),
    ("ws", "a  {%- if true -%}\n  b  {%- endif %}  c {# note -#}   d\n{{- ' e ' -}}\n f", {}),
    ("macro", "{% macro m(a, b='B', c=none) -%}\n<{{ a }}|{{ b }}|{{ c }}>\n{%- endmacro %}{{ m(1) }}{{ m('<', c='C') }}{{ m(a='x', b='y') }}", {}),
    ("urlenc", "{{ n|urlencode }}|{{ ('/accounts/' ~ 3)|urlencode }}|{{ 'a b&c=d/é'|urlencode }}|{{ none|urlencode }}", {"n": "Jean & Cie / 50%"}),
    ("int", "{{ '12'|int }}|{{ 'x'|int }}|{{ 3.9|int }}|{{ -3.9|int }}|{{ d|int }}|{{ (0.29 * 100)|int }}|{{ '4.5'|int }}", {"d": {"$dec": "-2.7"}}),
    ("tuples", "{% for title, funds in (('A', l), ('B', none)) if funds %}{{ title }}:{{ funds|length }}{% endfor %}|{{ (1, 2) }}|{{ () }}", {"l": [1, 2]}),
    ("compare_chain", "{{ 1 < 2 < 3 }}{{ 1 < 3 < 2 }}{{ a != '' }}{{ a == '' }}{{ 'b' > 'a' }}{{ [1,2] == [1,2] }}{{ (1,2) == [1,2] }}", {"a": {"$dec": "0.0"}}),
    ("andor", "{{ 0 or '' or 'x' }}|{{ 1 and 2 }}|{{ none and 1 }}|{{ [] or [1] }}|{{ not 0 }}", {}),
    ("concat", "{{ 'n°' ~ 3 ~ x }}|{{ ('/a/' ~ i)|urlencode }}", {"x": {"$dec": "1.10"}, "i": 7}),
    ("reverse", "{% for x in l|reverse %}{{ x }}{% endfor %}|{{ 'abc'|reverse }}", {"l": [1, 2, 3]}),
    ("markupfmt", "{{ ('%s' ~ '')|format(s) }}|{{ m|replace('b', '<i>') }}|{{ s|replace('<', m) }}", {"s": "<x>", "m": {"$markup": "<b>"}}),
]

EXTRA = {
    "base.html": "<t>{% block title %}T{% endblock %}</t>\n{% block content %}{% endblock %}\n<f>{{ footer }}</f>\n",
    "_m.html": "{% macro hi(n) %}hi {{ n }} {{ csrf }}{% endmacro %}\n{% macro copy(t, label='Copier') -%}\n<b data-c=\"{{ t }}\">{{ label }}</b>\n{%- endmacro %}",
    "_inc.html": "[inc {{ x }} {{ local }}]",
    "child.html": "{% extends \"base.html\" %}\n{% block title %}{{ name }}{% endblock %}\n{% from \"_m.html\" import hi, copy with context %}\n{% block content %}\n{% set local = 'L' %}{{ hi(name) }} {{ copy('a\"b') }}{% include \"_inc.html\" %}\n{% endblock %}\n",
}
TEMPLATE_CASES = [("child.html", {"name": "Zoé", "csrf": "tok", "footer": "pied", "x": 1})]


def decode(v):
    from decimal import Decimal
    from datetime import date, datetime
    from types import SimpleNamespace
    from markupsafe import Markup
    if isinstance(v, list):
        return [decode(x) for x in v]
    if isinstance(v, dict):
        if len(v) == 1:
            (k, x), = v.items()
            if k == "$dec": return Decimal(x)
            if k == "$date": return date.fromisoformat(x)
            if k == "$dt": return datetime.fromisoformat(x)
            if k == "$markup": return Markup(x)
            if k == "$tuple": return tuple(decode(i) for i in x)
            if k == "$float": return float(x)
            if k == "$obj": return SimpleNamespace(**{a: decode(b) for a, b in x.items()})
            if k == "$intdict": return {a: decode(b) for a, b in x}
        return {k: decode(x) for k, x in v.items()}
    return v


if __name__ == "__main__":
    if sys.argv[1] == "dump":
        json.dump({"cases": CASES, "extra": EXTRA, "templates": TEMPLATE_CASES}, sys.stdout, ensure_ascii=False)
    else:
        import jinja2
        env = jinja2.Environment(loader=jinja2.DictLoader(EXTRA), autoescape=True)
        out = []
        for name, src, ctx in CASES:
            try:
                out.append([name, env.from_string(src).render(**decode(ctx))])
            except Exception as exc:
                out.append([name, f"ERROR {type(exc).__name__}"])
        for name, ctx in TEMPLATE_CASES:
            out.append([name, env.get_template(name).render(**decode(ctx))])
        json.dump(out, sys.stdout, ensure_ascii=False, indent=0)
