# Change Log

## 0.1.0-beta

designed basic structure:

```text
src/
  main.rs    # Program entry point; parses the CLI and invokes the driver
  cli.rs     # Command-line argument parsing; converts user input into DriverConfig

  frontend/
  ir/
  runtime/
  interp/
  llvm/
  extensions/
  driver/
```

```
BF source ──► Lexer ──► Parser ──► IR
                                    │
                                    │
                       Optimizer ◄──┤
                                    │
             Interpreter backend ◄──┤
                                    │
                    LLVM backend ◄──┘
```