#!/bin/sh
# Builds the synthetic test programs with Open Watcom v2: set WATCOM to its folder. Each program
# goes to bin/NAME.EXE with its linker map bin/NAME.MAP (function addresses: ground truth for
# the analysis pipeline). The results are committed, so tests don't need the compiler.
set -e
cd "$(dirname "$0")"
: "${WATCOM:?set WATCOM to the Open Watcom v2 folder}"
case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) watcom=$(cygpath -u "$WATCOM"); PATH="$watcom/binnt64:$watcom/binnt:$PATH" ;;
    *) PATH="$WATCOM/binl64:$WATCOM/binl:$PATH" ;;
esac
export INCLUDE="$WATCOM/h" PATH
mkdir -p bin obj

# build NAME MODEL [compiler options] SOURCES...
build() {
    name=$1
    model=$2
    shift 2
    wcl -zq -bt=dos -m"$model" -fo=obj/ -fe=bin/"$name".EXE -fm=bin/"$name".MAP "$@"
}

build HELLO s src/hello.c
build CALLCONV s src/callconv.c
build FARDATA l src/fardata.c
build FILEIO s src/fileio.c
build MODE13 s src/mode13.c
build TIMER s src/timer.c
build FLOAT s -fpi87 src/float.c
build MEDIUM m src/medmain.c src/medlib.c
rm -rf obj
