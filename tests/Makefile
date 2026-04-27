.PHONY: build test clean

build:
	docker build -t rsync-rs:latest ..

test: build
	docker run --rm -v $(PWD):/tests rsync-rs:latest bash /tests/run_compat_tests.sh

docker-compose-test:
	docker-compose -f ../docker-compose.yml up --abort-on-container-exit

clean:
	docker rmi rsync-rs:latest 2>/dev/null || true
