# Dockerfile.validate
FROM busybox:latest

RUN echo "Running step 3"

WORKDIR /pipeline
RUN mkdir -p /pipeline/out

CMD sh -c 'echo "validation=passed" > /pipeline/out/validation.txt && echo "stage=validate" >> /pipeline/out/validation.txt && cat /pipeline/out/validation.txt'