FROM public.ecr.aws/docker/library/golang:1.26-bookworm AS build

WORKDIR /src
COPY go.mod go.sum main.go ./
RUN CGO_ENABLED=0 go build -trimpath -o /pg18-pgx-matrix .

FROM public.ecr.aws/docker/library/debian:bookworm-slim
COPY --from=build /pg18-pgx-matrix /pg18-pgx-matrix

ENTRYPOINT ["/pg18-pgx-matrix"]
