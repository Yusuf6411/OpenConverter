FROM gcr.io/oss-fuzz-base/base-builder-rust
RUN git clone --depth 1 https://github.com/Yusuf6411/OpenConverter $SRC/openconverter
WORKDIR $SRC/openconverter
COPY build.sh $SRC/
