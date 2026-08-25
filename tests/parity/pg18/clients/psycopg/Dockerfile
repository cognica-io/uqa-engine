FROM public.ecr.aws/docker/library/python:3.13-slim-bookworm

RUN python -m pip install --no-cache-dir "psycopg[binary]==3.3.4" "psycopg-pool==3.3.1"

COPY client.py /matrix/client.py

ENTRYPOINT ["python", "/matrix/client.py"]
