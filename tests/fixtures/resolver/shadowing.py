from manim import Square

x = Square
x()
x = 5
x()


def rebind():
    y = Square()
    y.shift()
    y = 5
    y.shift()
