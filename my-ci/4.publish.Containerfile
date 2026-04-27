# Dockerfile.publish
FROM busybox:latest

RUN echo "Running step 4"

WORKDIR /pipeline
RUN mkdir -p /pipeline/out

CMD sh -c 'echo "published=true" > /pipeline/out/result.txt && echo "stage=publish" >> /pipeline/out/result.txt && cat /pipeline/out/result.txt'