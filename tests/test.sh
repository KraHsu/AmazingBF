#!/usr/bin/env bash

set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_PATH="$TEST_DIR/../target/release/AmazingBF"

CASES_DIR="$TEST_DIR/cases"
TMP_DIR="$TEST_DIR/tmp"
BIN_TMP_DIR="$TMP_DIR/bin"
OUT_TMP_DIR="$TMP_DIR/out"

mkdir -p "$BIN_TMP_DIR" "$OUT_TMP_DIR"

MODE="${1:-all}"   # interp | compile | all

if [[ ! -x "$BIN_PATH" ]]; then
    echo "error: binary not found or not executable: $BIN_PATH"
    exit 1
fi

pass_count=0
fail_count=0

run_interp_test() {
    local bf_file="$1"
    local name
    name="$(basename "$bf_file" .bf)"
    
    local in_file="$CASES_DIR/$name.in"
    local ans_file="$CASES_DIR/$name.out"
    local out_file="$OUT_TMP_DIR/${name}.interp.out"
    
    if [[ ! -f "$in_file" || ! -f "$ans_file" ]]; then
        echo "[interp] $name: missing .in or .out"
        ((fail_count++))
        return
    fi
    
    if "$BIN_PATH" "$bf_file" < "$in_file" > "$out_file"; then
        if diff -u "$ans_file" "$out_file" > /dev/null; then
            echo "[interp] $name: PASS"
            ((++pass_count))
        else
            echo "[interp] $name: FAIL (output mismatch)"
            diff -u "$ans_file" "$out_file" || true
            ((++fail_count))
        fi
    else
        echo "[interp] $name: FAIL (runtime error)"
        ((++fail_count))
    fi
}

run_compile_test() {
    local bf_file="$1"
    local name
    name="$(basename "$bf_file" .bf)"
    
    local in_file="$CASES_DIR/$name.in"
    local ans_file="$CASES_DIR/$name.out"
    local exe_file="$BIN_TMP_DIR/$name"
    local out_file="$OUT_TMP_DIR/${name}.compile.out"
    
    if [[ ! -f "$in_file" || ! -f "$ans_file" ]]; then
        echo "[compile] $name: missing .in or .out"
        ((fail_count++))
        return
    fi
    
    rm -f "$exe_file" "$out_file"
    
    if "$BIN_PATH" "$bf_file" -m to-elf -o "$exe_file"; then
        chmod +x "$exe_file" || true
        
        # 按常见编译器习惯，编译后的程序直接运行即可
        if "$exe_file" < "$in_file" > "$out_file"; then
            if diff -u "$ans_file" "$out_file" > /dev/null; then
                echo "[compile] $name: PASS"
                ((++pass_count))
            else
                echo "[compile] $name: FAIL (output mismatch)"
                diff -u "$ans_file" "$out_file" || true
                ((++fail_count))
            fi
        else
            echo "[compile] $name: FAIL (compiled program runtime error)"
            ((++fail_count))
        fi
    else
        echo "[compile] $name: FAIL (compile error)"
        ((++fail_count))
    fi
}

shopt -s nullglob
bf_files=("$CASES_DIR"/*.bf)

if [[ ${#bf_files[@]} -eq 0 ]]; then
    echo "error: no .bf files found in $CASES_DIR"
    exit 1
fi

for bf_file in "${bf_files[@]}"; do
    case "$MODE" in
        interp)
            run_interp_test "$bf_file"
        ;;
        compile)
            run_compile_test "$bf_file"
        ;;
        all)
            run_interp_test "$bf_file"
            run_compile_test "$bf_file"
        ;;
        *)
            echo "usage: $0 [interp|compile|all]"
            exit 1
        ;;
    esac
done

echo
echo "passed: $pass_count"
echo "failed: $fail_count"

if [[ $fail_count -ne 0 ]]; then
    exit 1
fi
