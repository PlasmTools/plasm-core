# Program and class declaration constructors

`PythonDeclaration` is generated from the production declaration classifier.
Root admission and class-body dispatch consume that same closed vocabulary.
Classification identifies candidates; session-aware admission validates bases,
decorators, signatures and annotations before any executable plan is accepted.
Neither the Python module, class body nor decorator is executed.

The gate compiles existing live matrix witnesses, then inventories their outer
declarations. Compute bodies are not classified as class declarations. Full-module
negative examples exercise the module boundary directly, without a test wrapper.
Negative error labels are closed semantic expectations matched against concrete
compilation causes or retained checker diagnostic codes, not rendered prose.
All four canonical row compute input annotations have live witnesses. A separate
metamorphic check proves that class documentation leaves the semantic plan unchanged.

These rules preserve current admission, including the restriction on build return
annotations. They do not certify all compute bodies, callback compositions or
annotation/domain combinations; those remain separate semantic obligations.

```plasm-declaration-constructors
[
  {
    "operation": "program",
    "premise": "Exactly one undecorated, non-generic class directly derives only from Program, without class keywords or reserved/session-shadowing name.",
    "transfer": "Admit a static declaration; execute no module or class code.",
    "law": "BC-03",
    "witnesses": [
      "get"
    ],
    "invalid": [
      {
        "body": "class Example(Program):\n    def build(self):\n        return E.get(\"i1\")\nclass Other(Program):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "ProgramDeclarationCount",
        "module": true
      },
      {
        "body": "class Example(object):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "ProgramBaseShape",
        "module": true
      },
      {
        "body": "class Example(Program, object):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "ProgramBaseShape",
        "module": true
      },
      {
        "body": "@arbitrary\nclass Example(Program):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "ProgramBaseShape",
        "module": true
      },
      {
        "body": "class Program(Program):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "ReservedRootName",
        "module": true
      },
      {
        "body": "class ENTITY(Program):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "RootShadowsEntity",
        "module": true
      },
      {
        "body": "class Example[T](Program):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "ProgramBaseShape",
        "module": true
      },
      {
        "body": "class Example(Program, metaclass=type):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "ProgramBaseShape",
        "module": true
      },
      {
        "body": "class Example:\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "ProgramBaseInvalid",
        "module": true
      }
    ]
  },
  {
    "operation": "documentation",
    "premise": "A class-body expression is a string literal.",
    "transfer": "Erase documentation from executable semantics.",
    "law": "BC-03",
    "witnesses": [
      "root_build_statements"
    ],
    "invalid": [
      {
        "body": "class Example(Program):\n    value = 1\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "ClassExecutableState",
        "module": true
      },
      {
        "body": "class Example(Program):\n    print(\"executed\")\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "ClassExecutableState",
        "module": true
      },
      {
        "body": "class Example(Program):\n    class Nested:\n        pass\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "ClassExecutableState",
        "module": true
      }
    ]
  },
  {
    "operation": "build",
    "premise": "A synchronous build binds the Program receiver through upstream Python call binding. Additional parameters require scalar constant defaults; variadic ports and runtime decorators have no Program representation.",
    "transfer": "Supply its statement suite to the registered build-statement lowerer.",
    "law": "BC-03",
    "witnesses": [
      "get",
      "root_build_statements"
    ],
    "invalid": [
      {
        "body": "class Example(Program):\n    \"documentation\"\n",
        "error": "ProgramBuildMissing",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(self):\n        return E.get(\"i1\")\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "DuplicateBuildMethod",
        "module": true
      },
      {
        "body": "class Example(Program):\n    async def build(self):\n        return E.get(\"i1\")\n",
        "error": "MethodDeclarationShape",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "BuildInterfaceShape",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(self) -> str:\n        return E.get(\"i1\")\n",
        "error": "BuildInterfaceShape",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(this):\n        return E.get(\"i1\")\n",
        "error": "BuildReceiverShape",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(self, *args):\n        return E.get(\"i1\")\n",
        "error": "VariadicBuildInputs",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(self, **kwargs):\n        return E.get(\"i1\")\n",
        "error": "VariadicBuildInputs",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(self, *, flag):\n        return E.get(\"i1\")\n",
        "error": "ArgumentBinding",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build[T](self):\n        return E.get(\"i1\")\n",
        "error": "MethodDeclarationShape",
        "module": true
      }
    ]
  },
  {
    "operation": "compute",
    "premise": "A public synchronous @compute method has a bound self receiver and annotated or callsite-inferred materialization inputs. Upstream Python binding resolves positional-only, keyword-only and scalar constant defaults. Whole-body checking validates annotated or inferred return contracts against actual callsite inputs.",
    "transfer": "Extract source without evaluating decorators; resolve catalog-qualified annotations and prepare the sealed compute contract. Row contracts resolve at typed callsites.",
    "law": "BC-03",
    "witnesses": [
      "type_projected_integer",
      "compute_inferred_callsite_inputs",
      "text_bindings_row",
      "text_synthetic_count",
      "text_collection_report"
    ],
    "invalid": [
      {
        "body": "class Example(Program):\n    @compute()\n    def text(self, row: Row) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "ComputeDecoratorShape",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @staticmethod\n    @compute\n    def text(self, row: Row) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "ComputeDecoratorShape",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    async def text(self, row: Row) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "MethodDeclarationShape",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def text(self, row: Rows) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "ComputeRowsHandleAnnotation",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def text(self, row: Row) -> int:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "InvalidReturnType",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def text(self, row: dict) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "UnresolvedAttribute",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def _text(self, row: Row) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "ComputeDecoratorShape",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def text(self, row: Row = None) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "InvalidReturnType",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return row.title\n    @compute\n    def text(self, row: Row) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "DuplicateComputeMethod",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def text[T](self, row: Row) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "MethodDeclarationShape",
        "module": true
      }
    ]
  },
  {
    "operation": "helper",
    "premise": "An undecorated synchronous Program method binds self and independent typed DAG arguments.",
    "transfer": "Elaborate one scoped invocation through Monty argument binding and control flow, preserving ordered effects and existing authority.",
    "law": "BC-03",
    "witnesses": [
      "root_build_statements"
    ],
    "invalid": [
      {
        "body": "class Example(Program):\n    def _loop(self, row):\n        return self._loop(row)\n    def build(self):\n        return self._loop(E.get(\"i1\"))\n",
        "error": "RecursiveDagMethod",
        "module": true
      }
    ]
  }
]
```
