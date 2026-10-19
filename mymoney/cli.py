from __future__ import annotations

import argparse
import logging
import os


def main() -> None:
    parser = argparse.ArgumentParser(prog="mymoney", description="Gestionnaire de patrimoine auto-hébergé")
    sub = parser.add_subparsers(dest="command", required=True)
    serve = sub.add_parser("serve", help="Lance l'interface web")
    serve.add_argument("--host", default=os.environ.get("MYMONEY_HOST", "127.0.0.1"))
    serve.add_argument("--port", type=int, default=int(os.environ.get("MYMONEY_PORT", "8000")))
    args = parser.parse_args()

    logging.basicConfig(level=os.environ.get("MYMONEY_LOG_LEVEL", "INFO"), format="%(asctime)s %(levelname)s %(name)s: %(message)s")
    # Libraries can log URLs or payloads: keep them quiet so nothing sensitive reaches the container logs.
    for noisy in ("httpx", "httpcore", "ccxt", "woob", "pytr", "pdfminer", "urllib3"):
        logging.getLogger(noisy).setLevel(logging.WARNING)

    if args.command == "serve":
        import uvicorn

        from .web.app import create_app

        uvicorn.run(
            create_app(),
            host=args.host,
            port=args.port,
            proxy_headers=True,
            forwarded_allow_ips=os.environ.get("MYMONEY_TRUSTED_PROXIES", "127.0.0.1"),
            server_header=False,
            access_log=False,
        )


if __name__ == "__main__":
    main()
