"""Evaluate the narrow Python fixture without importing agent-written code."""
import ast
from urllib.parse import DefragResult, ParseResult, SplitResult, urldefrag, urlparse, urlsplit, urlunparse, urlunsplit


MAX_SOURCE_BYTES = 32 * 1024
MAX_AST_NODES = 256
SAFE_FUNCTIONS = {
    "urldefrag": urldefrag,
    "urlparse": urlparse,
    "urlsplit": urlsplit,
    "urlunparse": urlunparse,
    "urlunsplit": urlunsplit,
}
PARSED_RESULTS = (DefragResult, ParseResult, SplitResult)
URL_INPUTS = (
    "https://example.test/docs?q=browser#setup",
    "https://example.test/?q=one",
    "https://example.test/#start",
    "https://example.test/docs?q=one#first#second",
    "/relative/path?q=%23encoded#fragment",
)


class UnsafeImplementation(ValueError):
    pass


def expression(node, scope, imports):
    if isinstance(node, ast.Constant) and type(node.value) in (str, int, type(None)):
        return node.value
    if isinstance(node, ast.Name):
        if node.id not in scope:
            raise UnsafeImplementation(f"unknown name {node.id!r}")
        return scope[node.id]
    if isinstance(node, (ast.Tuple, ast.List)):
        values = [expression(item, scope, imports) for item in node.elts]
        return tuple(values) if isinstance(node, ast.Tuple) else values
    if isinstance(node, ast.Subscript):
        value = expression(node.value, scope, imports)
        if not isinstance(value, (str, tuple, list)):
            raise UnsafeImplementation("subscripts require a string or sequence")
        if isinstance(node.slice, ast.Slice):
            index = slice(*(expression(value, scope, imports) if value else None
                            for value in (node.slice.lower, node.slice.upper, node.slice.step)))
        else:
            index = expression(node.slice, scope, imports)
        if not isinstance(index, (int, slice)):
            raise UnsafeImplementation("only integer and slice subscripts are allowed")
        return value[index]
    if isinstance(node, ast.Attribute):
        value = expression(node.value, scope, imports)
        if isinstance(value, PARSED_RESULTS) and node.attr in value._fields:
            return getattr(value, node.attr)
        raise UnsafeImplementation(f"attribute access {node.attr!r} is not allowed")
    if isinstance(node, ast.Call):
        args = [expression(value, scope, imports) for value in node.args]
        if any(keyword.arg is None for keyword in node.keywords):
            raise UnsafeImplementation("expanded keyword arguments are not allowed")
        kwargs = {keyword.arg: expression(keyword.value, scope, imports) for keyword in node.keywords}
        if isinstance(node.func, ast.Name):
            if node.func.id in scope or node.func.id not in imports:
                raise UnsafeImplementation(f"call to {node.func.id!r} is not allowed")
            return imports[node.func.id](*args, **kwargs)
        if isinstance(node.func, ast.Attribute):
            receiver = expression(node.func.value, scope, imports)
            method = node.func.attr
            if isinstance(receiver, str) and method in ("partition", "rpartition", "rsplit", "split"):
                if kwargs:
                    raise UnsafeImplementation("string methods do not accept keywords here")
                return getattr(receiver, method)(*args)
            if isinstance(receiver, PARSED_RESULTS) and method == "_replace":
                if args or any(key not in receiver._fields for key in kwargs):
                    raise UnsafeImplementation("invalid parsed URL replacement")
                return receiver._replace(**kwargs)
            if isinstance(receiver, (ParseResult, SplitResult)) and method == "geturl":
                if args or kwargs:
                    raise UnsafeImplementation("geturl does not accept arguments")
                return receiver.geturl()
        raise UnsafeImplementation("dynamic calls are not allowed")
    raise UnsafeImplementation(f"expression {type(node).__name__} is not allowed")


def run_function(function, value, imports):
    argument = [*function.args.posonlyargs, *function.args.args][0].arg
    scope = {argument: value}
    body = list(function.body)
    if body and isinstance(body[0], ast.Expr) and isinstance(body[0].value, ast.Constant) \
            and isinstance(body[0].value.value, str):
        body.pop(0)
    if not body or not isinstance(body[-1], ast.Return):
        raise UnsafeImplementation("normalize_url must end with a return")
    for statement in body[:-1]:
        if not isinstance(statement, ast.Assign) or len(statement.targets) != 1 \
                or not isinstance(statement.targets[0], ast.Name):
            raise UnsafeImplementation(f"statement {type(statement).__name__} is not allowed")
        scope[statement.targets[0].id] = expression(statement.value, scope, imports)
    return expression(body[-1].value, scope, imports)


def check_normalize_url(path):
    """Return behavioral grading results without executing or importing the file."""
    try:
        if path.is_symlink() or not path.is_file():
            raise UnsafeImplementation("url_utils.py must be a regular file")
        raw = path.read_bytes()
        if len(raw) > MAX_SOURCE_BYTES:
            raise UnsafeImplementation("url_utils.py is too large for local grading")
        tree = ast.parse(raw.decode("utf-8"), filename="url_utils.py")
        if sum(1 for _ in ast.walk(tree)) > MAX_AST_NODES:
            raise UnsafeImplementation("url_utils.py is too complex for local grading")
        imports = {}
        functions = []
        for statement in tree.body:
            if isinstance(statement, ast.Expr) and isinstance(statement.value, ast.Constant) \
                    and isinstance(statement.value.value, str):
                continue
            if isinstance(statement, ast.ImportFrom) and statement.level == 0 and statement.module == "urllib.parse":
                for name in statement.names:
                    if name.name not in SAFE_FUNCTIONS:
                        raise UnsafeImplementation(f"import {name.name!r} is not allowed")
                    imports[name.asname or name.name] = SAFE_FUNCTIONS[name.name]
                continue
            if isinstance(statement, ast.FunctionDef) and statement.name == "normalize_url":
                functions.append(statement)
                continue
            raise UnsafeImplementation(f"top-level {type(statement).__name__} is not allowed")
        if len(functions) != 1:
            raise UnsafeImplementation("exactly one normalize_url function is required")
        function = functions[0]
        positional = [*function.args.posonlyargs, *function.args.args]
        if len(positional) != 1 or function.args.vararg or function.args.kwarg or function.args.kwonlyargs \
                or function.args.defaults or function.decorator_list or function.returns \
                or any(argument.annotation for argument in positional):
            raise UnsafeImplementation("normalize_url must accept exactly one required argument")
        for url in URL_INPUTS:
            actual = run_function(function, url, imports)
            expected = urldefrag(url)[0]
            if actual != expected:
                return False, f"restricted local check failed for {url!r}: expected {expected!r}, got {actual!r}"
        return True, f"restricted local check passed {len(URL_INPUTS)} URL cases without importing agent code"
    except (IndexError, OSError, OverflowError, RecursionError, SyntaxError, TypeError, UnicodeError,
            UnsafeImplementation, ValueError) as error:
        return False, f"restricted local check rejected the implementation: {error}"
