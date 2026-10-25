"""Write a set of made-up CSV, OFX and QIF files that exercise the corners of the readers (quoting, encodings, broken
files, every error message), to compare the Rust and the Python readers on them. All names and numbers are invented.

    python tools/gen_edge_cases.py <output dir>
"""

import os
import sys

out = sys.argv[1]
os.makedirs(out, exist_ok=True)
n = 0


def put(name: str, data):
    global n
    n += 1
    folder = os.path.join(out, f"{n:03d}")
    os.makedirs(folder, exist_ok=True)
    with open(os.path.join(folder, name), "wb") as fh:
        fh.write(data if isinstance(data, bytes) else data.encode("utf-8"))


# ------------------------------------------------------------------------------------------------- CSV
put("export.csv", "Banque Exemple;Compte courant\nIBAN : FR76 3000 4000 0312 3456 7890 143\n\nDate opération;Libellé;Débit;Crédit;Solde\n"
    "05/01/2026;CB CAFE DU COIN;4,50;;1 200,00\n06/01/2026;\"VIR SEPA \"\"LOYER\"\"\";650,00;;550,00\n07/01/2026;SALAIRE;;2 100,00;2 650,00\n"
    "bad;line;;;\n08/01/2026;FRAIS;1,00;;2 649,00\n")
put("cp1252.csv", "Date;Libellé;Montant\n02/02/2026;Café crème;-3,20\n03/02/2026;Remboursement été;15,00\n".encode("cp1252"))
put("latin.csv", b"Date;Libell\xe9;Montant\n02/02/2026;Caf\x81;-3,20\n")
put("comma.csv", 'date,description,amount,currency\n2026-03-01,"Coffee, large",-4.50,eur\n2026-03-02,Refund,"1,234.56",EUR\n2026-03-03,Other,12.00,usd\n')
put("tabs.tsv", "Date\tLabel\tAmount\n01.04.26\tShop\t-10,00\n02.04.26\tShop 2\t(5,00)\n03.04.26\tShop 3\t7,00-\n")
put("pipes.txt", "date|libelle|montant\n5 janv. 2026|Boulangerie|-1,20\n6 févr. 2026|Pharmacie|-8,90\n")
put("revolut.csv", "Type,Product,Started Date,Completed Date,Description,Amount,Fee,Currency,State,Balance\n"
    "CARD_PAYMENT,Current,2026-05-01 10:00:00,2026-05-02 09:00:00,Grocery,-25.10,0.00,EUR,COMPLETED,474.90\n"
    "TOPUP,Current,2026-05-03 10:00:00,,Top-up,100.00,0.00,EUR,PENDING,\n"
    "CARD_PAYMENT,Current,2026-05-04 10:00:00,2026-05-04 11:00:00,Taxi,-12.00,0.50,EUR,COMPLETED,462.40\n"
    "EXCHANGE,Current,2026-05-05 10:00:00,2026-05-05 10:00:00,Exchanged,-50.00,0.00,EUR,REVERTED,462.40\n")
put("revolut_multi.csv", "Type,Product,Started Date,Completed Date,Description,Amount,Fee,Currency,State,Balance\n"
    "CARD_PAYMENT,Current,2026-05-01,2026-05-02,A,-1.00,0.00,EUR,COMPLETED,10.00\nCARD_PAYMENT,Current,2026-05-01,2026-05-03,B,-2.00,0.00,USD,COMPLETED,8.00\n")
put("holdings.csv", "Symbol;Name;Quantity;Value;Currency\nbtc;Bitcoin;0,0125;800,00;eur\neth;;1,5;;\n;Nothing;1;2;EUR\n")
put("isin.csv", "Code ISIN,Nom,Quantité,Valorisation\nFR0000120271,Exemple SA,10,\"1 234,00\"\n")
put("nothing.csv", "a;b;c\n1;2;3\n")
put("mac.csv", "Date;Libellé;Montant\r01/01/2026;A;1,00\r02/01/2026;B;2,00\r")
put("bom.csv", "﻿Date;Libellé;Montant\n01/06/2026;Avec BOM;-9,99\n")
put("empty.csv", "")
put("newest_first.csv", "Date;Libellé;Montant;Solde\n10/07/2026;C;-1,00;97,00\n09/07/2026;B;-1,00;98,00\n08/07/2026;A;-1,00;99,00\n")
put("quoted_multiline.csv", 'Date;Libellé;Montant\n01/08/2026;"Ligne 1\nligne 2";-1,00\n02/08/2026;"x""y";2,00\n')
put("single_column.csv", "Montant\n1,00\n2,00\n")
put("quotes_sniff.csv", "'Date','Libellé','Montant'\n'01/09/2026','A','-1,00'\n")
put("only_header.csv", "Date;Montant\n")
put("spaces_header.csv", "  Date : ; Montant (EUR) \n01/10/2026;12,00\n")
put("debit_only.csv", "Date;Débit\n01/11/2026;5,00\n02/11/2026;0,00\n")
put("fee_zero.csv", "Date;Libellé;Montant;Frais\n01/12/2026;A;-5,00;0,00\n02/12/2026;B;-5,00;0,50\n")
put("weird_dates.csv", "Date;Montant\n2026-13-01;1,00\n31/02/2026;2,00\n01/01/26;3,00\n1/1/2026;4,00\n")

# ------------------------------------------------------------------------------------------------- OFX
SGML = ("OFXHEADER:100\r\nDATA:OFXSGML\r\nVERSION:102\r\nSECURITY:NONE\r\nENCODING:{enc}\r\nCHARSET:{cs}\r\nCOMPRESSION:NONE\r\n"
        "OLDFILEUID:NONE\r\nNEWFILEUID:NONE\r\n\r\n")


def stmt(trns: str, acct: str = "12345678901", bal: str = "1234.56", cur: str = "EUR", kind: str = "CHECKING") -> str:
    return (f"<STMTTRNRS><TRNUID>1<STATUS><CODE>0<SEVERITY>INFO</STATUS><STMTRS><CURDEF>{cur}<BANKACCTFROM><BANKID>30004"
            f"<BRANCHID>00031<ACCTID>{acct}<ACCTTYPE>{kind}</BANKACCTFROM><BANKTRANLIST><DTSTART>20260101<DTEND>20260131"
            f"{trns}</BANKTRANLIST><LEDGERBAL><BALAMT>{bal}<DTASOF>20260131</LEDGERBAL></STMTRS></STMTTRNRS>")


def trn(date="20260105", amt="-12.50", fitid="A1", name="CB BOULANGERIE", memo="05/01 PARIS", extra="") -> str:
    parts = ["<STMTTRN><TRNTYPE>DEBIT", f"<DTPOSTED>{date}" if date is not None else "", f"<TRNAMT>{amt}" if amt is not None else "",
             f"<FITID>{fitid}" if fitid is not None else "", f"<NAME>{name}" if name is not None else "", f"<MEMO>{memo}" if memo is not None else "", extra, "</STMTTRN>"]
    return "".join(parts)


def ofx(body: str, enc="USASCII", cs="1252", sonrs=True) -> str:
    son = "<SIGNONMSGSRSV1><SONRS><STATUS><CODE>0<SEVERITY>INFO</STATUS><DTSERVER>20260201<LANGUAGE>FRA</SONRS></SIGNONMSGSRSV1>" if sonrs else ""
    return SGML.format(enc=enc, cs=cs) + f"<OFX>{son}<BANKMSGSRSV1>{body}</BANKMSGSRSV1></OFX>\r\n"


put("basic.ofx", ofx(stmt(trn() + trn("20260107", "1500.00", "A2", "VIR SALAIRE", ""))).encode("cp1252"))
put("accents_1252.ofx", ofx(stmt(trn(name="CAFÉ DÉLICES", memo="crème brûlée"))).encode("cp1252"))
put("accents_utf8.ofx", ofx(stmt(trn(name="CAFÉ DÉLICES")), enc="UTF-8", cs="NONE").encode("utf-8"))
put("unicode.ofx", ofx(stmt(trn(name="NOËL")), enc="UNICODE", cs="NONE").encode("utf-8"))
put("charset_none.ofx", ofx(stmt(trn()), cs="NONE").encode("ascii"))
put("charset_8859.ofx", ofx(stmt(trn(name="ÉTÉ")), cs="8859-1").encode("latin-1"))
put("charset_850.ofx", ofx(stmt(trn(name="ÉTÉ")), cs="850").encode("cp850"))
put("bad_1252.ofx", ofx(stmt(trn(name="X"))).encode("cp1252").replace(b"<NAME>X", b"<NAME>\x81X"))
put("enc_other.ofx", ofx(stmt(trn()), enc="UTF-16").encode("ascii"))
put("no_encoding_accents.ofx", ("OFXHEADER:100\nDATA:OFXSGML\n\n" + "<OFX>" + stmt(trn(name="CRÈME")) + "</OFX>").encode("utf-8"))
put("xml.ofx", '<?xml version="1.0" encoding="UTF-8"?><?OFX OFXHEADER="200" VERSION="211"?>\n<OFX><BANKMSGSRSV1><STMTTRNRS><STMTRS>'
    "<CURDEF>EUR</CURDEF><BANKACCTFROM><ACCTID>999</ACCTID><ACCTTYPE>SAVINGS</ACCTTYPE></BANKACCTFROM><BANKTRANLIST>"
    "<STMTTRN><TRNTYPE>CREDIT</TRNTYPE><DTPOSTED>20260301120000.000[+1:CET]</DTPOSTED><TRNAMT>10.00</TRNAMT><FITID>X1</FITID>"
    "<NAME>INTERETS</NAME></STMTTRN></BANKTRANLIST></STMTRS></STMTTRNRS></BANKMSGSRSV1></OFX>")
put("xml_accents.ofx", '<?xml version="1.0"?>\n<OFX><BANKMSGSRSV1><STMTTRNRS><STMTRS><BANKTRANLIST><STMTTRN><DTPOSTED>20260301</DTPOSTED>'
    "<TRNAMT>1</TRNAMT><FITID>1</FITID><NAME>ÉPARGNE</NAME></STMTTRN></BANKTRANLIST></STMTRS></STMTTRNRS></BANKMSGSRSV1></OFX>")
put("bom_xml.ofx", '﻿<?xml version="1.0"?><OFX></OFX>')
put("cc.ofx", SGML.format(enc="USASCII", cs="1252") + "<OFX><CREDITCARDMSGSRSV1><CCSTMTTRNRS><TRNUID>2<STATUS><CODE>0<SEVERITY>INFO</STATUS>"
    "<CCSTMTRS><CURDEF>USD<CCACCTFROM><ACCTID>4970XXXX1234</CCACCTFROM><BANKTRANLIST>" + trn("20260110", "-3.00", "C1", "SHOP", "") +
    "</BANKTRANLIST><LEDGERBAL><BALAMT>-3.00<DTASOF>20260131</LEDGERBAL></CCSTMTRS></CCSTMTTRNRS></CREDITCARDMSGSRSV1></OFX>")
put("multi.ofx", ofx(stmt(trn(fitid="M1"), acct="111") + stmt(trn("20260120", "5.00", "M2", "B", "c"), acct="222", kind="SAVINGS")))
put("no_fitid.ofx", ofx(stmt(trn(fitid=None))))
put("empty_fitid.ofx", ofx(stmt(trn(fitid=""))))
put("null_amount.ofx", ofx(stmt(trn(amt="null") + trn(amt="-null", fitid="N2") + trn(amt="", fitid="N3"))))
put("bad_amount.ofx", ofx(stmt(trn(amt="abc"))))
put("no_amount.ofx", ofx(stmt(trn(amt=None))))
put("tz.ofx", ofx(stmt(trn("20260105003000[+1:CET]") + trn("20260105233000[-5:EST]", fitid="T2") + trn("20260105233000.5[-3.5:NST]", fitid="T3"))))
put("zero_date.ofx", ofx(stmt(trn("00000000"))))
put("bad_date.ofx", ofx(stmt(trn("20260230"))))
put("short_date.ofx", ofx(stmt(trn("2026013"))))
put("garbage_date.ofx", ofx(stmt(trn("2026-01-05"))))
put("entities.ofx", ofx(stmt(trn(name="A&amp;B &eacute;t&#233; &#x41; &foo; &#128; &#0; &", memo="x &lt;y&gt;"))))
put("empty_memo.ofx", ofx(stmt(trn(memo="") + trn(fitid="E2", name="   ", memo="   "))))
put("no_name.ofx", ofx(stmt(trn(name=None, memo=None))))
put("sonrs_no_code.ofx", SGML.format(enc="USASCII", cs="1252") + "<OFX><SIGNONMSGSRSV1><SONRS><STATUS><SEVERITY>INFO</STATUS></SONRS></SIGNONMSGSRSV1>"
    "<BANKMSGSRSV1>" + stmt(trn()) + "</BANKMSGSRSV1></OFX>")
put("sonrs_bad_code.ofx", ofx(stmt(trn())).replace("<CODE>0<SEVERITY>INFO</STATUS><DTSERVER>", "<CODE>zz<SEVERITY>INFO</STATUS><DTSERVER>"))
put("status_no_code.ofx", ofx(stmt(trn()).replace("<STATUS><CODE>0<SEVERITY>INFO</STATUS><STMTRS>", "<STATUS><SEVERITY>INFO</STATUS><STMTRS>")))
put("acctinfo.ofx", SGML.format(enc="USASCII", cs="1252") + "<OFX><SIGNUPMSGSRSV1><ACCTINFOTRNRS><ACCTINFORS><DTACCTUP>20260101"
    "<ACCTINFO><DESC>Livret Exemple<BANKACCTINFO><BANKACCTFROM><BANKID>1<ACCTID>777<ACCTTYPE>SAVINGS</BANKACCTFROM></BANKACCTINFO></ACCTINFO>"
    "<ACCTINFO><DESC><CCACCTINFO><CCACCTFROM><ACCTID>888</CCACCTFROM></CCACCTINFO></ACCTINFO>"
    "</ACCTINFORS></ACCTINFOTRNRS></SIGNUPMSGSRSV1><FI><ORG>Banque Exemple<FID>123</FI></OFX>")
put("acctinfo_ok.ofx", SGML.format(enc="USASCII", cs="1252") + "<OFX><SIGNUPMSGSRSV1><ACCTINFOTRNRS><ACCTINFORS>"
    "<ACCTINFO><DESC>Livret Exemple<BANKACCTINFO><BANKACCTFROM><ACCTID>777<ACCTTYPE>SAVINGS</BANKACCTFROM></BANKACCTINFO></ACCTINFO>"
    "</ACCTINFORS></ACCTINFOTRNRS></SIGNUPMSGSRSV1><FI><ORG>Banque Exemple<FID>123</FI></OFX>")
put("fi_empty_org.ofx", ofx(stmt(trn())).replace("</OFX>", "<FI><ORG></ORG><FID>1</FI></OFX>"))
INV = ("<INVSTMTMSGSRSV1><INVSTMTTRNRS><INVSTMTRS><CURDEF>EUR<INVACCTFROM><BROKERID>b.example<ACCTID>INV1</INVACCTFROM>"
       "<INVTRANLIST><DTSTART>20260101<DTEND>20260131{inv}</INVTRANLIST><INVPOSLIST><POSSTOCK><INVPOS><SECID><UNIQUEID>FR0000120271"
       "<UNIQUEIDTYPE>ISIN</SECID><UNITS>10<UNITPRICE>12,5<MKTVAL>125</INVPOS></POSSTOCK></INVPOSLIST><INVBAL><AVAILCASH>10.00</INVBAL>"
       "</INVSTMTRS></INVSTMTTRNRS></INVSTMTMSGSRSV1><SECLISTMSGSRSV1><SECLIST><STOCKINFO><SECINFO><SECID><UNIQUEID>FR0000120271"
       "</SECID><SECNAME>Exemple<TICKER>EX</SECINFO></STOCKINFO></SECLIST></SECLISTMSGSRSV1>")
put("inv_bank.ofx", SGML.format(enc="USASCII", cs="1252") + "<OFX>" + INV.format(inv="<INVBANKTRAN>" + trn(fitid="I1") + "<SUBACCTFUND>CASH</INVBANKTRAN>") + "</OFX>")
put("inv_buy.ofx", SGML.format(enc="USASCII", cs="1252") + "<OFX>" + INV.format(inv="<BUYSTOCK><INVBUY><INVTRAN><FITID>B1<DTTRADE>20260110</INVTRAN>"
    "<SECID><UNIQUEID>FR0000120271</SECID><UNITS>1<UNITPRICE>12.5<TOTAL>-12.5</INVBUY><BUYTYPE>BUY</BUYSTOCK>") + "</OFX>")
put("inv_nocur.ofx", SGML.format(enc="USASCII", cs="1252") + "<OFX>" + INV.format(inv="").replace("<CURDEF>EUR", "") + "</OFX>")
put("empty_balance.ofx", ofx(stmt(trn(), bal="")))
put("bad_balance.ofx", ofx(stmt(trn(), bal="n/a")))
put("eu_amounts.ofx", ofx(stmt(trn(amt="-1.234,56") + trn(amt="+12,00", fitid="E2") + trn(amt="1 025,53", fitid="E3") + trn(amt="10,000.50", fitid="E4"))))
put("header_no_colon.ofx", "OFXHEADER:100\r\nGARBAGE LINE\r\n\r\n<OFX></OFX>")
put("header_two_colons.ofx", "OFXHEADER:100:1\r\n\r\n<OFX></OFX>")
put("no_accounts.ofx", ofx(""))
put("not_ofx.ofx", "hello world, nothing here\n")
put("no_tag_at_all.qfx", "OFXHEADER:100\nENCODING:USASCII\n")
put("comment_cdata.ofx", ofx(stmt(trn(name="<!-- un commentaire -->", memo="<![CDATA[brut & <tel>]]>"))))
put("charref_quirk.ofx", ofx(stmt(trn(name="X&#12a\nY") + trn(fitid="Q2"))))
put("nested_name.ofx", ofx(stmt(trn(name="<B>gras</B>"))))
put("lowercase.ofx", ofx(stmt(trn())).replace("<STMTTRN>", "<stmttrn>").replace("</STMTTRN>", "</stmttrn>"))
put("text_lt.ofx", ofx(stmt(trn(memo="< 5 EUR"))))
put("dtuser.ofx", ofx(stmt(trn(extra="<DTUSER>20260230"))))
put("sic.ofx", ofx(stmt(trn(extra="<SIC>5812<CHECKNUM>42"))))
put("empty_sic.ofx", ofx(stmt(trn(extra="<SIC></SIC>"))))
put("no_trntype.ofx", ofx(stmt(trn().replace("<TRNTYPE>DEBIT", "<TRNTYPE></TRNTYPE>"))))
put("script.ofx", ofx(stmt(trn(name="<script>if (a<b) x</script>"))))
put("void.ofx", ofx(stmt(trn(name="A<br>B"))))
put("unclosed_comment.ofx", ofx(stmt(trn())).replace("</OFX>", "<!-- jamais fermé"))
put("big_amount.ofx", ofx(stmt(trn(amt="-0.00") + trn(amt="1e2", fitid="B2") + trn(amt="1_000", fitid="B3"))))

# ------------------------------------------------------------------------------------------------- QIF
put("basic.qif", "!Type:Bank\nD01/02/2026\nT-12.50\nPBoulangerie\nMpain\n^\nD03/02'26\nU1,500.00\nPSalaire\n^\nD\nT1\n^\n")
put("utf8.qif", "!Type:Bank\nD05/02/2026\nT-3,00\nPCafé crème\n^\n")
put("latin.qif", "!Type:Bank\nD05/02/2026\nT-3,00\nPCaf\xe9\n^\n".encode("latin-1"))
put("broken_utf8.qif", b"!Type:Bank\nD05/02/2026\nT-3,00\nPCaf\xc3\xa9 \xc3\x28 x\n^\n")
put("nothing.qif", "!Type:Bank\nPpas de date\n^\n")
put(".qif", "!Type:Bank\nD05/02/2026\nT1\n^\n")

print(n, "files")
