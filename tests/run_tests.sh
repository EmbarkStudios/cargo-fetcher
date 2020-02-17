#!/bin/bash
set -eu

# Spin up a minio container we'll use as our "backend"
export AWS_ACCESS_KEY_ID=ANTN35UAENTS5UIAEATD
export AWS_SECRET_ACCESS_KEY=TtnuieannGt2rGuie2t8Tt7urarg5nauedRndrur

export MINIO_ACCESS_KEY=$AWS_ACCESS_KEY_ID
export MINIO_SECRET_KEY=$AWS_SECRET_ACCESS_KEY
export MINIO_DOMAIN=localhost

if [ "$(command -v podman)" ]; then
    DOCKER=$(command -v podman)
else
    DOCKER=docker
fi

TAG="RELEASE.2020-02-07T23-28-16Z"

host_port="9000"
if [ -z "$($DOCKER ps -f "label=s3=minio_fetcher" -f status=running -q)" ]; then
    echo "pulling minio image..."
    $DOCKER pull "minio/minio:$TAG"

    echo "starting minio container..."
    container_id=$($DOCKER run \
        -t \
        -l minio_fetcher \
        -p 9000:9000 \
        --env MINIO_ACCESS_KEY \
        --env MINIO_SECRET_KEY \
        --env MINIO_DOMAIN \
        --rm \
        --detach \
        minio/minio:$TAG \
        server \
        /home/shared)

    echo "Started container $container_id"

    function cleanup() {
        rv=$?
        #$DOCKER kill "$container_id" || true
        #$DOCKER rm "$container_id" || true
        exit $rv
    }

    trap "cleanup" INT TERM EXIT
fi

echo "Using host port $host_port"

export S3_ENDPOINT="http://localhost:$host_port"

# The minio server takes up to 2 seconds to startup on my machine
# so just poll it here so the test setup is simpler
printf "waiting for minio"
while [[ "$(curl -s -o /dev/null -w ''%{http_code}'' localhost:$host_port)" != "403" ]]; do
    printf '.'
    sleep 1
done

printf "\n"
which_test=${1:-""}

RUST_BACKTRACE=1 RUST_LOG=cargo_fetcher=trace cargo test --release --features s3_test $which_test -- --nocapture
