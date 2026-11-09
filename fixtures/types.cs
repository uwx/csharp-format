using System;
using System.Collections.Generic;
namespace N
{
    public delegate void D<T>(T value) where T : class;
    public record R(int X, string Y);
    public record struct RS(int X);
    public class C<T> where T : struct, IComparable<T>
    {
        public event EventHandler E;
        public event EventHandler F { add { } remove { } }
        public int this[int i] { get { return i; } set { } }
        int IComparable<C<T>>.CompareTo(C<T> other) { return 0; }
        public static implicit operator int(C<T> c) { return 0; }
    }
    public static class Ext
    {
        public static void M(this string s) { }
    }
    public class Primary(int x, string y)
    {
        public int X { get; } = x;
        public string Y => y;
    }
}
