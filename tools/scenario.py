"""Play a scenario (a JSON list of operations) on the Python application and print what each one returns, then the
whole database, in a form the Rust side (`mymoney scenario`) prints too: both outputs must be equal.

    python tools/scenario.py <data dir> <scenario.json>        (run from python/)

Times that depend on the clock (created_at, updated_at...) and random names (blob files) are left out of the dump.
"""

import json
import sys
from datetime import date, datetime
from decimal import Decimal
from pathlib import Path

from sqlalchemy import text

from mymoney.adapters import reader_version
from mymoney.config import Settings
from mymoney.core import MyMoney
from mymoney.services import access, analytics, categories, coverage, loans, statements
from mymoney.services import counterparties as contacts
from mymoney.services import users as user_service
from mymoney.models import Account, Loan

SKIP = {"created_at", "updated_at", "imported_at", "uploaded_at", "last_login", "fetched_at", "blob_name"}


def plain(v):
    """The canonical form both sides print."""
    if isinstance(v, Decimal):
        return str(v)
    if isinstance(v, datetime):
        return v.strftime("%Y-%m-%d %H:%M:%S")
    if isinstance(v, date):
        return v.isoformat()
    if isinstance(v, dict):
        return {str(k): plain(x) for k, x in v.items()}
    if isinstance(v, (list, tuple, set)):
        return [plain(x) for x in v]
    return v


def run(core: MyMoney, op: dict):
    kind = op["op"]
    if kind == "upload":
        path = Path(op["file"])
        data = path.read_bytes()
        core.purge_quarantine()
        doc_id, existing = core.store_document(op.get("name") or path.name, data)
        if existing:
            return {"doc": doc_id, "state": "existing"}
        status = core.analyze_document(doc_id, None, data)
        verdict = core.check_duplicate(doc_id) if status == "parsed" else None
        imported = core.auto_import(doc_id) if status == "parsed" and verdict is None else None
        state = "duplicate" if verdict == "identical" else "differs" if verdict else "imported" if imported else status
        return {"doc": doc_id, "state": state, "imported": imported}
    if kind == "analyze":
        return core.analyze_document(op["doc"], op.get("adapter"))
    if kind == "import_default":
        return core.import_default(op["doc"], op.get("index", 0), op.get("force", False))
    if kind == "import":
        return core.import_statement(op["doc"], op.get("index", 0), op["form"])
    if kind == "check_obvious":
        return core._check_obvious(op["doc"], apply=op.get("apply", False))
    if kind == "pending":
        return {"reasons": core.pending_reasons(), "next": list(core.next_pending(op.get("after")))}
    if kind == "invalidate":
        return core.invalidate_documents(op["docs"])
    if kind == "undo":
        return core.undo_import(op["doc"])
    if kind == "replace":
        return core.replace_document(op["doc"], op.get("index", 0))
    if kind == "keep":
        return core.keep_duplicate(op["doc"])
    if kind == "rebuild":
        return core.rebuild_imports()
    if kind == "save_account":
        return core.save_account(op.get("id"), op["data"])
    if kind == "delete_account":
        return core.delete_account(op["id"])
    if kind == "delete_document":
        return core.delete_document(op["doc"])
    if kind == "portfolio":
        p = core.portfolio(with_history=True)
        return {
            "total": p.total, "assets": p.assets, "debts": p.debts, "by_category": p.by_category,
            "accounts": [[v.account.id, v.cash_eur, v.total, v.warnings] for v in p.accounts],
            "history": [[d.isoformat(), v] for d, v in p.history][-3:],
        }
    with core.db.session() as s:
        if kind == "review":
            out = []
            for st in [*(core.review_document(op["doc"]) or {"plans": []})["plans"]]:
                out.append({
                    "account": st.account.id if st.account else None, "candidates": [a.id for a in st.candidates],
                    "statuses": st.statuses(), "issues": st.issues, "clean": st.clean, "overlaps": [o.id for o in st.overlaps],
                    "already": st.already_imported.id if st.already_imported else None, "ask_holder": st.ask_holder,
                    "newer_balance": st.newer_balance, "loan_existing": st.loan_existing, "loan_unverified": st.loan_unverified,
                    "dedup": None if st.dedup is None else [st.dedup.identical, len(st.dedup.extra), [t.id for t in st.dedup.missing],
                                                             st.dedup.opening, st.dedup.closing, st.dedup.new_count, st.dedup.probable_count],
                })
            return out
        if kind == "spending_report":
            ids = [a.id for a in s.query(Account).order_by(Account.id)]
            r = analytics.spending_report(s, ids, date.fromisoformat(op["start"]), date.fromisoformat(op["end"]), op.get("internal", False),
                                          op.get("direction", "out"), op.get("category"))
            return {
                "total": r.total, "out": r.total_out, "in": r.total_in, "saved": r.saved,
                "categories": [[c.id, c.label, c.total, c.count, c.share] for c in r.categories], "months": r.months,
                "category": None if r.category is None else [r.category.id, r.category.total],
                "merchants": [[m.key, m.label, m.contact_id, m.total, m.count, m.category, m.ruled] for m in r.merchants],
                "operations": [[t.id, n] for t, n in r.operations],
                "top": [[m.key, m.label, m.total, m.count, m.category, m.ruled] for m in r.top_merchants],
                "fees": [r.fees.operations_total, r.fees.operations_count, r.fees.summaries_total, r.fees.interest_paid, r.fees.refunded,
                         [[f.account_id, f.period_start, f.period_end, f.charged, f.refunded, f.net, f.operations, f.operations_count, f.gap]
                          for f in r.fees.summaries]],
            }
        if kind == "flows":
            ids = [a.id for a in s.query(Account).order_by(Account.id)]
            start, end = date.fromisoformat(op["start"]), date.fromisoformat(op["end"])
            flows = analytics.monthly_flows(s, ids, start, end, op.get("internal", False))
            sp = analytics.spending(s, ids, start, end, op.get("internal", False))
            rank = lambda rs: [[r.label, r.total, r.count, r.contact_id] for r in rs]  # noqa: E731
            return {"flows": [[f.month, f.income, f.expense, f.net] for f in flows], "kinds": rank(sp.by_kind), "out": rank(sp.top_out),
                    "in": rank(sp.top_in), "total": [sp.total_out, sp.total_in]}
        if kind == "history":
            today = date.fromisoformat(op["today"])
            out = {}
            for a in s.query(Account).order_by(Account.id):
                h = analytics.account_history(s, a, a.balance, today)
                out[str(a.id)] = [[d.isoformat(), v] for d, v in sorted(h.items())][-5:] + [len(h)]
            return out
        if kind == "loan":
            loan = s.query(Loan).filter(Loan.account_id == op["account"]).one_or_none()
            if loan is None:
                return None
            plan = loans.build(s, loan, date.fromisoformat(op["today"]))
            sm = dict(plan.summary)
            if sm.get("next") is not None:
                sm["next"] = [sm["next"].date, sm["next"].total]
            return {"rows": [[r.kind, r.date, r.number, r.capital, r.interest, r.insurance, r.total, r.remaining, r.source, r.status, r.confirmed, r.note]
                             for r in plan.rows], "checks": [[c.status, c.text] for c in plan.checks], "summary": sm}
        if kind == "gaps":
            return [[g.account_id, g.start, g.end, g.ack_id, g.label, g.days] for g in coverage.all_gaps(s, include_acknowledged=True)]
        if kind == "acknowledge":
            coverage.acknowledge(s, op["account"], date.fromisoformat(op["start"]), date.fromisoformat(op["end"]), op.get("note"))
            return None
        if kind == "coverage":
            rows, broken = statements.coverage(s, op["account"])
            return [[r.id for r in rows], broken]
        if kind == "unresolved":
            return [[u.kind, u.key, u.name, u.iban, u.count, u.total_in, u.total_out, u.last, sorted(u.accounts)]
                    for u in contacts.unresolved(s, op.get("shops", False))]
        if kind == "identify":
            return contacts.identify(s, op["data"]).id
        if kind == "contacts":
            return [[c.contact.id, c.contact.name, c.count, c.total_in, c.total_out, c.last, [i.value for i in c.contact.identifiers]]
                    for c in contacts.summaries(s)]
        if kind == "set_rule":
            categories.set_rule(s, op["key"], op["category"])
            return None
        if kind == "categorize":
            from mymoney.models import Transaction

            rules = categories.load_rules(s)
            return [[t.id, categories.categorize(t, rules)] for t in s.query(Transaction).order_by(Transaction.id)]
    raise ValueError(f"opération inconnue : {kind}")


def dump(core: MyMoney) -> dict:
    out = {}
    with core.db.session() as s:
        tables = [r[0] for r in s.execute(text("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name"))]
        for t in tables:
            cols = [r[1] for r in s.execute(text(f'PRAGMA table_info("{t}")'))]
            keep = [c for c in cols if c not in SKIP]
            rows = s.execute(text(f'SELECT {", ".join(chr(34) + c + chr(34) for c in keep)} FROM "{t}" ORDER BY rowid')).all()
            out[t] = [dict(zip(keep, [x if not isinstance(x, str) else x.replace(reader_version(), "READER") for x in r])) for r in rows]
    return out


def main() -> int:
    data_dir, script = sys.argv[1], json.loads(Path(sys.argv[2]).read_text())
    settings = Settings(data_dir=Path(data_dir), argon2_memory_kib=8192, argon2_time_cost=1)
    core = MyMoney(settings)
    core.setup("moi@example.org", "Moi", "un mot de passe assez long")
    with core.db.session() as s:
        me = user_service.identity(user_service.get_by_email(s, "moi@example.org"))
    results = []
    with access.as_user(me):
        for op in script:
            try:
                results.append({"op": op["op"], "ok": plain(run(core, op))})
            except Exception as exc:
                results.append({"op": op["op"], "error": str(exc)})
    print(json.dumps({"results": results, "db": plain(dump(core))}, ensure_ascii=False, indent=1).replace(reader_version(), "READER"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
