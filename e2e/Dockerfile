FROM python:3.11-slim

WORKDIR /e2e

COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY runner.py ./

ENTRYPOINT ["python", "/e2e/runner.py"]
