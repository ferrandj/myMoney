from __future__ import annotations

from .base import Connector
from .enablebanking import EnableBankingConnector
from .exchanges import ExchangeConnector
from .gocardless import GoCardlessConnector
from .traderepublic import TradeRepublicConnector
from .wallets import BitcoinWalletConnector, EvmWalletConnector, SolanaWalletConnector
from .woob_connector import WoobConnector

CONNECTORS: dict[str, type[Connector]] = {
    c.type: c
    for c in (
        EnableBankingConnector,
        WoobConnector,
        TradeRepublicConnector,
        ExchangeConnector,
        EvmWalletConnector,
        BitcoinWalletConnector,
        SolanaWalletConnector,
        GoCardlessConnector,
    )
}


def get_connector(connector_type: str) -> type[Connector]:
    try:
        return CONNECTORS[connector_type]
    except KeyError:
        raise ValueError(f"Connecteur inconnu : {connector_type}") from None
