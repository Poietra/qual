from manim import Square


def wrap(value):
    return value


Square()

sq = Square()

first = second = Square()


def helper():
    return Square()


@wrap(1)
def decorated():
    pass


with Square() as ctx:
    pass

if Square():
    Square()

total = Square(
    Square(),
)
