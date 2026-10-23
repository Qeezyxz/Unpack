"""Remove the two eval sites (new Function) from the esbuild bundle of node-unrar-js 2.0.2
so the app can run with a CSP that has no 'unsafe-eval'. Mirrors Emscripten's own
DYNAMIC_EXECUTION=0 closure-based invoker. Run after re-bundling:
  npx esbuild .../esm/index.esm.js --bundle --format=esm --outfile=src/vendor/unrar/unrar.bundle.js
  python3 scripts/patch-unrar.py
"""
import re, sys
p = sys.argv[1] if len(sys.argv) > 1 else "src/vendor/unrar/unrar.bundle.js"
s = open(p).read()
a = '''      return new Function("body", "return function " + name + '() {\\n    "use strict";    return body.apply(this, arguments);\\n};\\n')(body);'''
assert a in s, "createNamedFunction not found"
s = s.replace(a, '''      // Unpack patch: no eval (CSP without 'unsafe-eval').
      var fn = function() { "use strict"; return body.apply(this, arguments); };
      try { Object.defineProperty(fn, "name", { value: name }); } catch (e) {}
      return fn;''')
i = s.index('      var invokerFnBody = "return function "')
j = s.index('      return invokerFunction;\n', i) + len('      return invokerFunction;\n')
s = s[:i] + '''      // Unpack patch: closure-based invoker, no runtime code generation (CSP without 'unsafe-eval').
      var expected = argCount - 2;
      return function() {
        if (arguments.length !== expected) {
          throwBindingError("function " + humanName + " called with " + arguments.length + " arguments, expected " + expected + " args!");
        }
        var destructors = needsDestructorStack ? [] : null;
        var wired = [];
        if (isClassMethodFunc) wired.push(argTypes[1].toWireType(destructors, this));
        for (var k = 0; k < expected; ++k) wired.push(argTypes[k + 2].toWireType(destructors, arguments[k]));
        var rv = cppInvokerFunc.apply(null, [cppTargetFunc].concat(wired));
        if (needsDestructorStack) {
          runDestructors(destructors);
        } else {
          for (var m = isClassMethodFunc ? 1 : 2; m < argTypes.length; ++m) {
            if (argTypes[m].destructorFunction !== null) argTypes[m].destructorFunction(wired[isClassMethodFunc ? m - 1 : m - 2]);
          }
        }
        if (returns) return argTypes[0].fromWireType(rv);
      };
''' + s[j:]
assert "new Function" not in s and "new_(Function" not in s
open(p, "w").write(s)
print("patched", p)
