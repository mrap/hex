.PHONY: install test smoke all

install:
	bash install.sh

test:
	./venv/bin/python3 -m pytest tests/ -v --tb=short

smoke:
	docker build -t hex-events-test -f tests/Dockerfile.install .
	docker run --rm hex-events-test

all: install test smoke
