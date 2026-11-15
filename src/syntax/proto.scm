(comment) @comment
(string) @string
["\"proto2\"" "\"proto3\""] @string
[(int_lit) (float_lit)] @number
[(true) (false)] @boolean

[(type) (key_type) (message_name) (enum_name) (service_name)] @type
(rpc_name) @function
(field (identifier) @property)
(oneof_field (identifier) @property)
(map_field (identifier) @property)
(enum_field (identifier) @constant)

[
  "syntax" "edition" "package" "import" "option"
  "message" "enum" "service" "rpc" "returns" "stream"
  "extend" "extensions" "oneof" "map" "reserved" "to" "max"
  "optional" "required" "repeated" "public" "weak"
] @keyword
["(" ")" "[" "]" "{" "}" "<" ">"] @punctuation.bracket
[";" "," "." ":"] @punctuation.delimiter
["=" "+" "-"] @operator
