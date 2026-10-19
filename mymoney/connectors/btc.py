"""Minimal, dependency-free Bitcoin helpers: BIP32 public derivation and address encoding.

Only public keys are ever handled here. Pure Python keeps the Docker image portable
to every NAS architecture (no native extension to compile).
"""

from __future__ import annotations

import hashlib
import hmac
import struct

# secp256k1
P = 2**256 - 2**32 - 977
N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
G = (
    0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
    0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8,
)


def _add(p1, p2):
    if p1 is None:
        return p2
    if p2 is None:
        return p1
    if p1[0] == p2[0] and (p1[1] + p2[1]) % P == 0:
        return None
    if p1 == p2:
        lam = 3 * p1[0] * p1[0] * pow(2 * p1[1], -1, P) % P
    else:
        lam = (p2[1] - p1[1]) * pow(p2[0] - p1[0], -1, P) % P
    x = (lam * lam - p1[0] - p2[0]) % P
    return x, (lam * (p1[0] - x) - p1[1]) % P


def _mul(k: int, point=G):
    result = None
    while k:
        if k & 1:
            result = _add(result, point)
        point = _add(point, point)
        k >>= 1
    return result


def _decompress(data: bytes):
    x = int.from_bytes(data[1:], "big")
    y = pow((pow(x, 3, P) + 7) % P, (P + 1) // 4, P)
    if y % 2 != data[0] % 2:
        y = P - y
    return x, y


def _compress(point) -> bytes:
    return bytes([2 + (point[1] & 1)]) + point[0].to_bytes(32, "big")


# Base58Check
_B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58decode_check(text: str) -> bytes:
    num = 0
    for char in text:
        num = num * 58 + _B58.index(char)
    raw = num.to_bytes((num.bit_length() + 7) // 8, "big")
    raw = b"\0" * (len(text) - len(text.lstrip("1"))) + raw
    payload, checksum = raw[:-4], raw[-4:]
    if hashlib.sha256(hashlib.sha256(payload).digest()).digest()[:4] != checksum:
        raise ValueError("Somme de contrôle base58 invalide")
    return payload


def b58encode_check(payload: bytes) -> str:
    raw = payload + hashlib.sha256(hashlib.sha256(payload).digest()).digest()[:4]
    num = int.from_bytes(raw, "big")
    out = ""
    while num:
        num, rem = divmod(num, 58)
        out = _B58[rem] + out
    return "1" * (len(raw) - len(raw.lstrip(b"\0"))) + out


# RIPEMD-160 (OpenSSL 3 often ships without it, so it is implemented here)
def _ripemd160(data: bytes) -> bytes:
    try:
        return hashlib.new("ripemd160", data).digest()
    except ValueError:
        pass
    r1 = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 7, 4, 13, 1, 10, 6, 15, 3, 12, 0, 9, 5, 2, 14, 11, 8,
          3, 10, 14, 4, 9, 15, 8, 1, 2, 7, 0, 6, 13, 11, 5, 12, 1, 9, 11, 10, 0, 8, 12, 4, 13, 3, 7, 15, 14, 5, 6, 2,
          4, 0, 5, 9, 7, 12, 2, 10, 14, 1, 3, 8, 11, 6, 15, 13]
    r2 = [5, 14, 7, 0, 9, 2, 11, 4, 13, 6, 15, 8, 1, 10, 3, 12, 6, 11, 3, 7, 0, 13, 5, 10, 14, 15, 8, 12, 4, 9, 1, 2,
          15, 5, 1, 3, 7, 14, 6, 9, 11, 8, 12, 2, 10, 0, 4, 13, 8, 6, 4, 1, 3, 11, 15, 0, 5, 12, 2, 13, 9, 7, 10, 14,
          12, 15, 10, 4, 1, 5, 8, 7, 6, 2, 13, 14, 0, 3, 9, 11]
    s1 = [11, 14, 15, 12, 5, 8, 7, 9, 11, 13, 14, 15, 6, 7, 9, 8, 7, 6, 8, 13, 11, 9, 7, 15, 7, 12, 15, 9, 11, 7, 13, 12,
          11, 13, 6, 7, 14, 9, 13, 15, 14, 8, 13, 6, 5, 12, 7, 5, 11, 12, 14, 15, 14, 15, 9, 8, 9, 14, 5, 6, 8, 6, 5, 12,
          9, 15, 5, 11, 6, 8, 13, 12, 5, 12, 13, 14, 11, 8, 5, 6]
    s2 = [8, 9, 9, 11, 13, 15, 15, 5, 7, 7, 8, 11, 14, 14, 12, 6, 9, 13, 15, 7, 12, 8, 9, 11, 7, 7, 12, 7, 6, 15, 13, 11,
          9, 7, 15, 11, 8, 6, 6, 14, 12, 13, 5, 14, 13, 13, 7, 5, 15, 5, 8, 11, 14, 14, 6, 14, 6, 9, 12, 9, 12, 5, 15, 8,
          8, 5, 12, 9, 12, 5, 14, 6, 8, 13, 6, 5, 15, 13, 11, 11]
    k1 = [0x00000000, 0x5A827999, 0x6ED9EBA1, 0x8F1BBCDC, 0xA953FD4E]
    k2 = [0x50A28BE6, 0x5C4DD124, 0x6D703EF3, 0x7A6D76E9, 0x00000000]
    mask = 0xFFFFFFFF

    def f(j, x, y, z):
        if j < 16:
            return x ^ y ^ z
        if j < 32:
            return (x & y) | (~x & z)
        if j < 48:
            return (x | ~y) ^ z
        if j < 64:
            return (x & z) | (y & ~z)
        return x ^ (y | ~z)

    def rol(x, n):
        return ((x << n) | (x >> (32 - n))) & mask

    msg = data + b"\x80" + b"\0" * ((55 - len(data)) % 64) + struct.pack("<Q", len(data) * 8)
    h = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0]
    for offset in range(0, len(msg), 64):
        x = struct.unpack("<16I", msg[offset : offset + 64])
        a1, b1, c1, d1, e1 = h
        a2, b2, c2, d2, e2 = h
        for j in range(80):
            t = (rol((a1 + (f(j, b1, c1, d1) & mask) + x[r1[j]] + k1[j // 16]) & mask, s1[j]) + e1) & mask
            a1, e1, d1, c1, b1 = e1, d1, rol(c1, 10), b1, t
            t = (rol((a2 + (f(79 - j, b2, c2, d2) & mask) + x[r2[j]] + k2[j // 16]) & mask, s2[j]) + e2) & mask
            a2, e2, d2, c2, b2 = e2, d2, rol(c2, 10), b2, t
        t = (h[1] + c1 + d2) & mask
        h[1] = (h[2] + d1 + e2) & mask
        h[2] = (h[3] + e1 + a2) & mask
        h[3] = (h[4] + a1 + b2) & mask
        h[4] = (h[0] + b1 + c2) & mask
        h[0] = t
    return struct.pack("<5I", *h)


def hash160(data: bytes) -> bytes:
    return _ripemd160(hashlib.sha256(data).digest())


# Bech32 (BIP173), for native segwit v0 addresses
_BECH32 = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"


def _polymod(values) -> int:
    gen = [0x3B6A57B2, 0x26508E6D, 0x1EA119FA, 0x3D4233DD, 0x2A1462B3]
    chk = 1
    for v in values:
        top = chk >> 25
        chk = (chk & 0x1FFFFFF) << 5 ^ v
        for i in range(5):
            chk ^= gen[i] if (top >> i) & 1 else 0
    return chk


def _convertbits(data: bytes, frombits: int, tobits: int) -> list[int]:
    acc, bits, out = 0, 0, []
    for value in data:
        acc = (acc << frombits) | value
        bits += frombits
        while bits >= tobits:
            bits -= tobits
            out.append((acc >> bits) & ((1 << tobits) - 1))
    if bits:
        out.append((acc << (tobits - bits)) & ((1 << tobits) - 1))
    return out


def segwit_address(hrp: str, version: int, program: bytes) -> str:
    data = [version] + _convertbits(program, 8, 5)
    expanded = [ord(c) >> 5 for c in hrp] + [0] + [ord(c) & 31 for c in hrp]
    checksum = _polymod(expanded + data + [0] * 6) ^ 1
    data += [(checksum >> 5 * (5 - i)) & 31 for i in range(6)]
    return hrp + "1" + "".join(_BECH32[d] for d in data)


# BIP32 public derivation
class ExtendedPublicKey:
    def __init__(self, chain_code: bytes, point):
        self.chain_code = chain_code
        self.point = point

    @classmethod
    def parse(cls, text: str) -> "ExtendedPublicKey":
        raw = b58decode_check(text)
        if len(raw) != 78 or raw[45] not in (2, 3):
            raise ValueError("Clé publique étendue invalide")
        return cls(raw[13:45], _decompress(raw[45:]))

    @property
    def public_key(self) -> bytes:
        return _compress(self.point)

    def child(self, index: int) -> "ExtendedPublicKey":
        if index >= 2**31:
            raise ValueError("Dérivation renforcée impossible depuis une clé publique")
        digest = hmac.new(self.chain_code, self.public_key + index.to_bytes(4, "big"), hashlib.sha512).digest()
        tweak = int.from_bytes(digest[:32], "big")
        if tweak >= N:
            raise ValueError("Index de dérivation invalide")
        return ExtendedPublicKey(digest[32:], _add(_mul(tweak), self.point))


def p2pkh(pub: bytes) -> str:
    return b58encode_check(b"\x00" + hash160(pub))


def p2sh_p2wpkh(pub: bytes) -> str:
    return b58encode_check(b"\x05" + hash160(b"\x00\x14" + hash160(pub)))


def p2wpkh(pub: bytes) -> str:
    return segwit_address("bc", 0, hash160(pub))
