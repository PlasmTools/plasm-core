# Program and class declaration constructors

`PythonDeclaration` is generated from the production declaration classifier.
Root admission and class-body dispatch consume that same closed vocabulary.
Classification identifies candidates; session-aware admission validates bases,
decorators, signatures and annotations before any executable plan is accepted.
Neither the Python module, class body nor decorator is executed.

The gate compiles existing live matrix witnesses, then inventories their outer
declarations. Compute bodies are not classified as class declarations. Full-module
negative examples exercise the module boundary directly, without a test wrapper.
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
        "body": "import os\nclass Example(Program):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "only the datetime module is admitted",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(self):\n        return E.get(\"i1\")\nclass Other(Program):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "expected exactly one Program subclass",
        "module": true
      },
      {
        "body": "class Example(object):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "root must derive directly and only from Program",
        "module": true
      },
      {
        "body": "class Example(Program, object):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "root must derive directly and only from Program",
        "module": true
      },
      {
        "body": "@arbitrary\nclass Example(Program):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "root must derive directly and only from Program",
        "module": true
      },
      {
        "body": "class Program(Program):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "reserved root name",
        "module": true
      },
      {
        "body": "class ENTITY(Program):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "root cannot shadow a session entity",
        "module": true
      },
      {
        "body": "class Example[T](Program):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "root must derive directly and only from Program",
        "module": true
      },
      {
        "body": "class Example(Program, metaclass=type):\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "root must derive directly and only from Program",
        "module": true
      },
      {
        "body": "class Example:\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "root must derive directly from Program",
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
        "error": "class state and executable class bodies are not admitted",
        "module": true
      },
      {
        "body": "class Example(Program):\n    print(\"executed\")\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "class state and executable class bodies are not admitted",
        "module": true
      },
      {
        "body": "class Example(Program):\n    class Nested:\n        pass\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "class state and executable class bodies are not admitted",
        "module": true
      }
    ]
  },
  {
    "operation": "build",
    "premise": "Exactly one synchronous undecorated build(self), without return annotations, generics or additional/default/variadic parameters.",
    "transfer": "Supply its statement suite to the registered build-statement lowerer.",
    "law": "BC-03",
    "witnesses": [
      "get",
      "root_build_statements"
    ],
    "invalid": [
      {
        "body": "class Example(Program):\n    \"documentation\"\n",
        "error": "Program requires build(self)",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(self):\n        return E.get(\"i1\")\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "duplicate build method",
        "module": true
      },
      {
        "body": "class Example(Program):\n    async def build(self):\n        return E.get(\"i1\")\n",
        "error": "expected a synchronous method",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def build(self):\n        return E.get(\"i1\")\n",
        "error": "build decorators and return annotations",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(self) -> str:\n        return E.get(\"i1\")\n",
        "error": "build decorators and return annotations",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(this):\n        return E.get(\"i1\")\n",
        "error": "method requires self",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(self, value=1):\n        return E.get(\"i1\")\n",
        "error": "method requires self",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(self, /):\n        return E.get(\"i1\")\n",
        "error": "method requires self",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(self: object):\n        return E.get(\"i1\")\n",
        "error": "method requires self",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(self, *args):\n        return E.get(\"i1\")\n",
        "error": "method requires self",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(self, **kwargs):\n        return E.get(\"i1\")\n",
        "error": "method requires self",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build(self, *, flag):\n        return E.get(\"i1\")\n",
        "error": "method requires self",
        "module": true
      },
      {
        "body": "class Example(Program):\n    def build[T](self):\n        return E.get(\"i1\")\n",
        "error": "expected a synchronous method without type parameters",
        "module": true
      }
    ]
  },
  {
    "operation": "compute",
    "premise": "A public synchronous method has exactly bare @compute, self and one required positional typed input, and a supported return annotation. Canonical inputs are Row, Value[eN], list[Row] or list[Value[eN]]. Value annotations are checked against the unique input column at the callsite.",
    "transfer": "Extract source without evaluating decorators; resolve catalog-qualified annotations and prepare the sealed compute contract. Row contracts resolve at typed callsites.",
    "law": "BC-03",
    "witnesses": [
      "type_projected_integer",
      "text_bindings_row",
      "text_synthetic_count",
      "text_collection_report"
    ],
    "invalid": [
      {
        "body": "class Example(Program):\n    def text(self, row: Row) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "only build and @compute methods",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute()\n    def text(self, row: Row) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "only build and @compute methods",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @staticmethod\n    @compute\n    def text(self, row: Row) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "only build and @compute methods",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    async def text(self, row: Row) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "expected a synchronous method",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def text(self, row) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "compute requires an input annotation",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def text(self, row: Row):\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "compute requires a return annotation",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def text(self, row: Row) -> int:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "Return type does not match returned value",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def text(self, row: dict) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "unknown Plasm return type dict",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def _text(self, row: Row) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "only build and @compute methods",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def text(self, row: Row = None) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "method requires self",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return row.title\n    @compute\n    def text(self, row: Row) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "duplicate compute method",
        "module": true
      },
      {
        "body": "class Example(Program):\n    @compute\n    def text[T](self, row: Row) -> str:\n        return row.title\n    def build(self):\n        return self.text(E.get(\"i1\"))\n",
        "error": "expected a synchronous method without type parameters",
        "module": true
      }
    ]
  }
]
```
