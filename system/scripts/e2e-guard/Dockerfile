FROM mcr.microsoft.com/playwright/python:v1.52.0-noble

WORKDIR /e2e-guard
COPY verify.py .

RUN pip install --no-cache-dir playwright && playwright install chromium

ENTRYPOINT ["python3", "verify.py"]
