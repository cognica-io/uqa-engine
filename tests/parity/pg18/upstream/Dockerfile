# PostgreSQL 18.4, also used by the Apache AGE oracle.
FROM postgres@sha256:22c89fe0d0f507606260237fd55e51f6137f58b2d5bcf6152242b96d9fe8f9a4

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       build-essential postgresql-server-dev-18=18.4-1.pgdg13+1 python3 locales \
    && rm -rf /var/lib/apt/lists/* \
    && echo 'en_US.UTF-8 UTF-8' > /etc/locale.gen \
    && locale-gen

COPY postgresql-18.4.tar.bz2 /tmp/postgresql-18.4.tar.bz2
RUN echo '81a81ec695fb0c7901407defaa1d2f7973617154cf27ba74e3a7ab8e64436094  /tmp/postgresql-18.4.tar.bz2' | sha256sum -c - \
    && tar -xjf /tmp/postgresql-18.4.tar.bz2 -C /opt \
    && rm /tmp/postgresql-18.4.tar.bz2

COPY harness.py source.json inventory.json coverage.json regress.mk postgresql.conf /harness/
RUN test "$(pg_config --version)" = 'PostgreSQL 18.4 (Debian 18.4-1.pgdg13+1)' \
    && make -C /harness -f regress.mk \
    && make -C /harness -f regress.mk install \
    && make -C /opt/postgresql-18.4/contrib/spi USE_PGXS=1 \
    && make -C /opt/postgresql-18.4/contrib/spi USE_PGXS=1 install \
    && mkdir /work && chown postgres:postgres /work

USER postgres
WORKDIR /work
ENV LANG=C.UTF-8
ENV LC_ALL=C.UTF-8
ENTRYPOINT ["python3", "/harness/harness.py", "container-run"]
