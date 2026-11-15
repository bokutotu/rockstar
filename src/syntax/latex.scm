; Commands are either named command nodes or anonymous tokens in a command field.
(command_name) @function
(_ command: _ @function)

[(line_comment) (block_comment) (comment_environment)] @comment
(begin name: (curly_group_text) @type)
(end name: (curly_group_text) @type)
(path) @string
(uri) @link_uri
(placeholder) @variable.parameter
(argc) @number

(part text: (curly_group) @title)
(chapter text: (curly_group) @title)
(section text: (curly_group) @title)
(subsection text: (curly_group) @title)
(subsubsection text: (curly_group) @title)

["{" "}" "[" "]" "(" ")"] @punctuation.bracket
["$" "$$" "\\(" "\\)" "\\[" "\\]"] @punctuation.special
["^" "_" "=" "+" "-" "/" "*" "<" ">"] @operator
