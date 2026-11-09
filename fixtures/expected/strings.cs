class A
{
    void M()
    {
        var a = "simple";
        var b = @"verbatim ""quoted""";
        var c = "escape \n \t \" end";
        var d = $"interp {a} and {b.Length}";
        var e = $@"verb interp {a}";
        var f = """
                raw
                "quoted"
                """;
        var g = $$"""raw {{a}} brace""";
        char h = '\'';
        var i = "unicode \u1234 and emoji 😀";
    }
}