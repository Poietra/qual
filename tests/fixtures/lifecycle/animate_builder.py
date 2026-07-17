from manim import PI, RIGHT, Scene, Square


class Builder(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        self.play(sq.animate.shift(RIGHT).rotate(PI))


class StaleBuilder(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        anim = sq.animate.shift(RIGHT)
        sq.shift(RIGHT)
        self.play(anim)


class Overwritten(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        first = sq.animate.shift(RIGHT)
        second = sq.animate.rotate(PI)
        self.play(first)
        self.play(second)
