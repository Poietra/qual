from manim import SVGMobject


def build(name):
    direct = SVGMobject("assets/valid.svg")
    declared = SVGMobject("assets/doctype.svg")
    dynamic = SVGMobject(name)
    return direct, declared, dynamic
