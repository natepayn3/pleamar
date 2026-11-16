-- At each `x` it writes down what `x.v` says; with `text:x.v`, what it was told, one by one.
local seen, heard = {}, {}
on("x", function()
    seen[#seen + 1] = text["x.v"]
    text.out = table.concat(seen, "|")
end)
on("text:x.v", function(v)
    heard[#heard + 1] = v
    text.heard = table.concat(heard, "|")
end)
