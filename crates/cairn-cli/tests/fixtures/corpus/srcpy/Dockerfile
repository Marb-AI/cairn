# The Python side needs no hop: compose names the module directly. Here for symmetry with
# srcgo, so the corpus does not accidentally test only the resolvable shape.
FROM python:3.13-slim
WORKDIR /app
COPY . .
EXPOSE 9102
CMD ["python3", "-m", "alerting.worker"]
