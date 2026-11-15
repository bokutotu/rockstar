(comment) @comment
(string_value) @string
[(int_value) (float_value)] @number
(boolean_value) @boolean
(null_value) @constant
(enum_value) @constant
(variable) @variable

(field (name) @property)
(field_definition (name) @property)
(argument (name) @property)
(object_field (name) @property)
(input_value_definition (name) @property)
(alias (name) @property)
(named_type (name) @type)
(object_type_definition (name) @type)
(interface_type_definition (name) @type)
(input_object_type_definition (name) @type)
(scalar_type_definition (name) @type)
(enum_type_definition (name) @type)
(union_type_definition (name) @type)
(operation_definition (name) @function)
(fragment_name (name) @function)
(directive (name) @attribute)

[
  "query" "mutation" "subscription" "fragment" "on"
  "schema" "type" "interface" "implements" "input"
  "enum" "scalar" "union" "extend" "directive" "repeatable"
] @keyword
["(" ")" "[" "]" "{" "}"] @punctuation.bracket
[":" "..."] @punctuation.delimiter
["!" "$" "@" "=" "|" "&"] @operator
