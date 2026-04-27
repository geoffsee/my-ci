# Dockerfile.extract
FROM busybox:latest

RUN echo "Running step 1"

WORKDIR /pipeline
RUN mkdir -p /pipeline/out

CMD sh -c 'echo "raw_input=$(date)" > /pipeline/out/raw.txt && cat /pipeline/out/raw.txt'