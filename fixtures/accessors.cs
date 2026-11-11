class A
{
    int F;
    int P { get { return 1; } }
    int Q { get; }
    int R { get { } set { } }
    void M() { }
    void N()
    {
    }

    int S { get => 1; }
    int T { get; set => F = value; }
    int U
    {
        get => 1;
        set => F = value;
    }
}
