# Dockerfile.transform
FROM busybox:latest

RUN echo "Running step 2"

WORKDIR /pipeline
RUN mkdir -p /pipeline/out

CMD sh -c 'echo "transformed=true" > /pipeline/out/transformed.txt && echo "stage=transform" >> /pipeline/out/transformed.txt && cat /pipeline/out/transformed.txt'