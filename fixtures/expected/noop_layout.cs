// Leading comment block.
//
// A second paragraph of the file header.

using System;

namespace Fixture
{
    public sealed class Layout
    {
        private readonly int[] _items = [];

        // An expression body keeps the call chain one level past the arrow line.
        private int[] Ordered()
            => _ordered ??= _items
                .Where(item => item > 0)
                .OrderByDescending(item => item)
                .ToArray();

        private int[]? _ordered;

        public string Describe(int value)
        {
            return value is 0
                ? ""
                : Describe(value - 1)?.ToUpperInvariant()
                  ?? "";
        }

        public void Show()
        {
            Console.WriteLine(
                "x",
                () => _items.Length > 0
                      && _items[0] != 0
                      && (_items[1] == 0 || _items[2] == 0),
                "y"
            );
        }

        public bool Any()
        {
            return _items.Length > 0
                   && (_items.Any(item => item == 1)
                       || _items.Any(item => item == 2));
        }

        public int Count
        {
            get => _items.Length;
            set => _count = value;
        }

        public int Total { get; set; }

        private int _count;

        #region Helpers

        private void Reset()
        {
            _count = 0;
        }

        #endregion
    }
}