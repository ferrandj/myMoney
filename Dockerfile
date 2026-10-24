ARG PYTHON_IMAGE=python:3.13-slim
FROM ${PYTHON_IMAGE} AS build
ENV PIP_NO_CACHE_DIR=1 PIP_DISABLE_PIP_VERSION_CHECK=1
WORKDIR /src
COPY pyproject.toml README.md ./
COPY mymoney ./mymoney
RUN python -m venv /opt/venv \
 && /opt/venv/bin/pip install . \
 && find /opt/venv -name "__pycache__" -type d -prune -exec rm -rf {} +

FROM ${PYTHON_IMAGE}
ARG VERSION=dev
LABEL org.opencontainers.image.title="myMoney" \
      org.opencontainers.image.description="Gestionnaire de patrimoine auto-hébergé, chiffré au repos" \
      org.opencontainers.image.source="https://github.com/ferrandj/myMoney"
RUN apt-get update \
 && apt-get upgrade -y --no-install-recommends \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --gid 10001 app \
 && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin app \
 && mkdir -p /data \
 && chown 10001:10001 /data \
 && chmod 700 /data
COPY --from=build /opt/venv /opt/venv
ENV PATH=/opt/venv/bin:$PATH \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    HOME=/tmp \
    MYMONEY_DATA_DIR=/data \
    MYMONEY_HOST=0.0.0.0 \
    MYMONEY_PORT=8000 \
    MYMONEY_VERSION=${VERSION}
USER 10001:10001
WORKDIR /tmp
VOLUME ["/data"]
EXPOSE 8000
HEALTHCHECK --interval=60s --timeout=5s --start-period=20s \
  CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8000/healthz', timeout=4)" || exit 1
ENTRYPOINT ["mymoney", "serve"]
