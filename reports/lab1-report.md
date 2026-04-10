# Lab1 实验报告

## 一、实验目标

为 teac 编译器的前端（Parser 阶段）添加三项新特性：

| 特性 | 类型 | 测试数 |
|------|------|--------|
| f32 浮点类型 | 必做 | 5 |
| `as` 类型转换 | 必做 | （含在浮点测试中） |
| `for` 循环 | 三选一 | 5 |

**本次 Lab1 的边界**：只涉及 Parser 阶段（源码 → AST），不涉及 IR 生成、代码优化、汇编生成。Parser 只负责"记录语法结构"，不做任何语义执行。

---

## 二、Parser 的职责边界

Parser 阶段做的事非常明确：把源码文本转成一棵结构化的数据树（AST），仅此而已。

以 `as` 类型转换为例：

```rust
let f:f32 = x as f32;
```

Parser 解析后得到：

```rust
CastExpr {
    unit: ExprUnit::Id("x"),          // 被转换的值
    cast_to: TypeSpecifier::Float,     // 目标类型是 f32
}
```

Parser **不做**的事：
- 不检查 `x` 是否真的是 `i32`
- 不生成任何转换指令
- 不知道 `f32` 在内存中怎么表示

这些都是 IR 生成阶段的工作。如果以后要支持 `f64`，需要改四个地方：

```
tealang.pest     → 加 kw_f64 规则
ast/types.rs     → BuiltIn 枚举加 F64 变体
parser/decl.rs   → parse_type_spec 加 Rule::kw_f64 分支
src/ir/          → 处理 cast_to 是 F64 时生成对应转换指令
```

前三步是 Parser 的改动，第四步才是"实际执行逻辑"，完全分离。

---

## 三、完整解析流程：Pest → Parser → AST

以 `for i in 0..10 { sum = sum + i; }` 为例，走完整链路。

### 阶段一：Pest 词法/语法分析

`tealang.pest` 定义了语法规则，Pest 在编译期读取它，自动生成解析代码。运行时，Pest 把输入字符串切分成一棵 **Pair 树**，每个节点记录：匹配了哪条规则、原始文本是什么。

```
for_stmt
├── kw_for       "for "
├── identifier   "i"
├── kw_in        "in "
├── range_bound
│   └── num      "0"
├── dot_dot      ".."
├── range_bound
│   └── num      "10"
├── lbrace       "{"
├── code_block_stmt
│   └── assignment_stmt
│       ├── left_val   "sum"
│       └── right_val  "sum + i"
└── rbrace       "}"
```

Pest 只做**结构识别**，所有节点值都还是原始字符串，没有任何语义。

关键语法规则（本次新增）：

```pest
float_literal = @{ ASCII_DIGIT* ~ "." ~ ASCII_DIGIT+ }
dot_dot       = @{ ".." }                         // 原子规则，不允许中间有空格
kw_f32   = @{ "f32" ~ &(...) }
kw_as    = @{ "as" ~ WHITESPACE }
kw_for   = @{ "for" ~ WHITESPACE }
kw_in    = @{ "in" ~ WHITESPACE }

cast_expr   = { expr_unit ~ (kw_as ~ type_spec)? }
range_bound = { (arith_expr) | fn_call | num | identifier }
for_stmt    = { kw_for ~ identifier ~ kw_in ~ range_bound ~ dot_dot
                ~ range_bound ~ lbrace ~ code_block_stmt* ~ rbrace }

// 两处有顺序要求：
// expr_unit 中 float_literal 必须在 num 之前（否则 3.14 被拆成 3 + .14）
// range_bound 中 fn_call 必须在 identifier 之前（否则 get_limit() 只匹配到 get_limit）
```

### 阶段二：ParseContext 把 Pair 树转成 AST

`parser/mod.rs` 的 `ParseContext::parse()` 是入口，逐层调用各模块的方法。

**调用路径**（以 for 循环为例）：

```
parse()                            ← mod.rs，顶层入口
  └─ parse_fn_def()                ← decl.rs
       └─ parse_code_block_stmt()  ← stmt.rs，遍历函数体
            └─ parse_for_stmt()    ← stmt.rs（Lab1 新增）
                 ├─ parse_range_bound()   解析 "0" → RangeBound::Num(0)
                 ├─ parse_range_bound()   解析 "10" → RangeBound::Num(10)
                 └─ parse_code_block_stmt()  递归解析循环体
                      └─ parse_assignment_stmt()
                           └─ parse_arith_expr()   ← expr.rs
                                └─ parse_arith_term()
                                     └─ parse_cast_expr()  （Lab1 新增）
                                          └─ parse_expr_unit()
```

`parse_for_stmt` 的核心逻辑：

```rust
fn parse_for_stmt(&self, pair: Pair) -> ParseResult<Box<ast::ForStmt>> {
    let mut iter_var = String::new();
    let mut start_bound = None;
    let mut end_bound = None;
    let mut stmts = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::identifier  => iter_var = inner.as_str().to_string(),
            Rule::range_bound => {
                // 第一次出现是 start，第二次是 end
                let bound = self.parse_range_bound(inner)?;
                if start_bound.is_none() { start_bound = Some(bound); }
                else                     { end_bound   = Some(bound); }
            }
            Rule::code_block_stmt => stmts.push(*self.parse_code_block_stmt(inner)?),
            _ => {}   // kw_for, kw_in, dot_dot, lbrace, rbrace 直接跳过
        }
    }

    Ok(Box::new(ast::ForStmt { iter_var, start, end, stmts }))
}
```

### 阶段三：AST 数据结构

经过上述转换，得到纯粹的 Rust 数据结构：

```rust
CodeBlockStmt {
    inner: For(ForStmt {
        iter_var: "i",
        start: RangeBound::Num(0),
        end:   RangeBound::Num(10),
        stmts: [
            CodeBlockStmt {
                inner: Assignment(AssignmentStmt {
                    left_val:  LeftVal { Id("sum") },
                    right_val: RightVal {
                        ArithExpr(
                            ArithBiOpExpr { op: Add,
                                left:  ExprUnit::Id("sum"),
                                right: ExprUnit::Id("i"),
                            }
                        )
                    }
                })
            }
        ]
    })
}
```

这棵树里没有任何字符串，全是强类型的枚举变体，方便下游（IR 生成）模式匹配处理。

### 阶段四：AST 输出（--emit ast）

`Display` 和 `DisplayAsTree` 两个 trait 负责打印。

- `Display`：单行紧凑格式，嵌入到父节点的行内（如 `ForStmt i in 0..10` 里的 `0` 和 `10`）
- `DisplayAsTree`：多行树状格式，每个节点独占一行，用 `├─` / `└─` / `│` 表示层级

两者关系：`DisplayAsTree` 打印节点头时，对子值调用 `Display`（单行内联）；对子节点调用 `DisplayAsTree`（递归展开）。

最终 `--emit ast` 输出：

```
└─FnDef main
   ├─VarDeclStmt
   │  └─sum = 0
   ├─ForStmt i in 0..10
   │  └─AssignmentStmt
   │     ├─LeftVal
   │     │  └─Id sum
   │     └─RightVal
   │        └─ArithExpr
   │           └─ArithBiOpExpr Add
   │              ├─ArithExpr
   │              │  └─ExprUnit
   │              │     └─Id(sum)
   │              └─ArithExpr
   │                 └─ExprUnit
   │                    └─Id(i)
   ├─ForStmt i in 1..6
   └─ReturnStmt 0
```

---

## 四、各特性实现细节

### f32 浮点类型

**Grammar**：新增 `float_literal = @{ ASCII_DIGIT* ~ "." ~ ASCII_DIGIT+ }`，在 `expr_unit` 中必须排在 `num` 之前，以及新增 `kw_f32`，在 `type_spec` 中排在 `kw_i32` 之前。

**AST**：`BuiltIn::Float`（`types.rs`），`ExprUnitInner::Float(f32)`（`expr.rs`）。

**Parser**：`parse_float()` 把文本转 `f32`，在 `parse_expr_unit` 中识别 `float_literal` 和 `-float_literal`，在 `parse_type_spec` 中识别 `kw_f32`。

### as 类型转换

**Grammar**：新增 `cast_expr = { expr_unit ~ (kw_as ~ type_spec)? }`，并将 `arith_term` 的操作单元从 `expr_unit` 改为 `cast_expr`，使 `as` 优先级高于 `*` 和 `/`。

**AST**：`CastExpr { unit: Box<ExprUnit>, cast_to: Box<TypeSpecifier> }`，`ExprUnitInner::Cast(Box<CastExpr>)`。

**Parser**：`parse_cast_expr()` 解析后，无 `as` 时直接返回 `expr_unit`，有 `as T` 时包装成 `Cast`。`parse_arith_term()` 改为调用 `parse_cast_expr()`。

### for 循环

**Grammar**：新增 `dot_dot = @{ ".." }`（原子规则防止 `. .` 误匹配），`range_bound`（四种边界形式，fn_call 排在 identifier 之前），`for_stmt`，并在 `code_block_stmt` 中加入 `for_stmt`。

**AST**：`RangeBound` 枚举（四个变体），`ForStmt` 结构体，`CodeBlockStmtInner::For`。

**Parser**：`parse_range_bound()` 处理四种边界，`parse_for_stmt()` 按出现顺序收集 iter_var、start、end 和 stmts。

---

## 五、测试

测试文件（`.tea` 源文件和测试函数）由老师提前写好，任务是让编译器能正确解析它们。

所有新特性的测试只测 `--emit ast`，不涉及 IR 生成和汇编：

```rust
// tests/tests.rs 中每个测试的结构
fn test_ast_parse(test_name: &str, must_contain: &[&str]) {
    // 运行: cargo run -- tests/<name>/<name>.tea --emit ast
    // 检查: 退出码为 0，stderr 为空，stdout 包含指定标识符
}
```

运行命令：

```bash
cargo test float_    # 5 个浮点测试
cargo test for_      # 5 个 for 循环测试
cargo test           # 全部 50 个测试
```

### 测试结果

| 测试集 | 通过 | 总数 | 说明 |
|--------|------|------|------|
| 原有测试 | 30 | 30 | 全部通过，无回归 |
| float_* | 5 | 5 | 全部通过 |
| for_* | 5 | 5 | 全部通过 |
| array_2d_* + array_3d + attention | 0 | 5 | 未实现（多维数组，另一三选一选项） |
| struct_method_* | 0 | 5 | 未实现（impl 块，另一三选一选项） |
| **合计** | **40** | **50** | |

---

## 六、改动文件清单

| 文件 | 改动内容 |
|------|---------|
| `src/tealang.pest` | 新增 8 条规则，修改 4 条已有规则 |
| `src/ast/types.rs` | `BuiltIn` 加 `Float` |
| `src/ast/expr.rs` | 加 `CastExpr`，`ExprUnitInner` 加 `Float`、`Cast` |
| `src/ast/stmt.rs` | 加 `RangeBound`、`ForStmt`，`CodeBlockStmtInner` 加 `For` |
| `src/ast.rs` | 导出新增类型 |
| `src/parser/common.rs` | 加 `parse_float()` |
| `src/parser/decl.rs` | `parse_type_spec` 加 `kw_f32` 分支 |
| `src/parser/expr.rs` | 加 `parse_cast_expr()`，修改 `parse_arith_term()`、`parse_expr_unit()` |
| `src/parser/stmt.rs` | 加 `parse_for_stmt()`、`parse_range_bound()`，修改 `parse_code_block_stmt()` |
| `src/ast/display.rs` | 加 `BuiltIn::Float`、`ExprUnitInner::Float/Cast`、`RangeBound`、`ForStmt` 的 Display |
| `src/ast/tree.rs` | 加 `ForStmt` 的 DisplayAsTree，`ExprUnit` 加 `Float`/`Cast` 分支，`CodeBlockStmtInner` 加 `For` 分支 |
| `src/ir/gen/function_gen.rs` | 加两行存根防止编译报错（IR 不实现）|
