# Selectors

Every op targets objects by **selector**, never by array index. Indices shift under the agent's
feet the moment anything is reordered; ids do not.

## Grammar

```
selector   := term ("," term)*                 # comma = union
term       := source? predicate*
source     := "#" ID                           # exact id
            | "@" NAME                         # exact name (may match several)
            | TYPE                             # type keyword: layer, path, text, group, node, mesh, …
            | "*"                              # everything in the target document
predicate  := "[" KEY OP VALUE "]"             # attribute filter
            | ":" PSEUDO                       # positional / state filter
OP         := "=" | "!=" | "^=" | "$=" | "*=" | ">" | "<"
PSEUDO     := first | last | visible | hidden | locked | empty | selected | nth(N)
```

A selector may be prefixed with a document: `logo:#mark` addresses `#mark` in document `logo`.
Without a prefix it resolves against `--doc`, or the active document.

## Examples

```bash
#lyr_sky                      # the layer with that exact id
@sky                          # every object named "sky"
layer[type=text]              # all text layers
layer[type=text][opacity<1]   # …that are not fully opaque
path[fill=#fb8500]            # every path with that fill
node[name^=bolt_]             # nodes whose name starts with bolt_
group:first                   # the first group in z-order
*:hidden                      # everything currently invisible
logo:#mark                    # cross-document reference
#title, #subtitle             # union of two
```

## Resolution rules

- **Zero matches is an error**, exit code `3`, not a silent no-op. The error lists the candidates
  that *do* exist in the document, so the agent can correct without another round trip:

  ```
  error: selector '#sky' matched 0 objects in doc_main
    available layers: #bg, #sky-grad, #title, #badge-mark
    did you mean: #sky-grad
  ```

- Ops declare their arity. `raster.layer.set` accepts many matches and applies to all;
  `vector.path.boolean --a` requires exactly one and errors on ambiguity, again listing matches.
- Order is document order (z-order for layers, tree order for nodes), stable across runs.
- Matches inside collapsed groups are included; `:child-of(#grp)` narrows when that matters.
- Selector resolution is a pure query — `dpaint inspect --select '<sel>'` returns exactly what an
  op would act on, so an agent can check a selector before committing a destructive edit.
